# Campaigns

Tally has three campaign weights:

- **Local worklist campaigns are one committed JSON file.** `tally campaign
  scaffold IDENTITY` writes a minimal admissible worklist, and `tally campaign
  arm OWNER/REPO WORKLIST` registers it against the checkout's own remote base
  branch. No forge issue graph, no spec corpus, and no module declaration is
  involved, and every later amendment is a push. [A small worklist, end to
  end](small-worklist.md) walks that lifecycle on an ordinary repository; the
  rest of this page is the reference behind it.
- **Ad-hoc campaigns are forge-native.** One GitHub master issue contains the
  campaign configuration and ordered DAG. Its native sub-issues contain the
  exact per-task briefs. `tally campaign arm ISSUE-URL` registers that durable
  object without a Nix edit, generation build, fleet deploy, or orchestration
  commit in the product repository.
- **Recurring campaigns are declarative.** `services.tally.campaigns` remains
  the right surface when the same label, mention, repository policy, and
  worklist discovery should be estate configuration.

All three use the same shipped, bounded, stateless `spec-build` reconciler.
Marked merged pull requests and automated checkpoint refs are durable
completion facts, and tally witnesses every observation and gate.

Keep those roles separate:

- **The selected container is the work source.** A recurring campaign reads its
  versioned tasks artifact from the exact fetched remote-base commit. An ad-hoc
  campaign executes only the exact manifest and native sub-issue bodies admitted
  by its most recent local `arm`, while each pass separately witnesses the live
  remote-base revision.
- **GitHub is intake, steering, state, and projection.** Manual `arm` is the
  explicit intent boundary for ad-hoc work; an exact mention is that boundary
  for a recurring campaign. Merged implementation pull requests and
  content-and-exact-base-bound checkpoint receipts are the completion facts read by
  every later pass. Only snapshotted comments authored by locally allowed actors
  steer an ad-hoc agent attempt; receipts and evidence project each
  reconciliation.
- **tally is the workflow engine.** It validates and witnesses the worklist,
  selects the dependency-ready frontier, creates isolated worktrees, runs
  deterministic gates and checkpoint commands, and serializes re-gated merges.

The module deliberately supplies mechanism, not project policy. Repository
owners still choose the corpus shape, gates, adapter, label, trusted actors, and
when a corpus is frozen.

## Arm an ad-hoc issue campaign

The Home Manager module installs the generic campaign pools, packaged flow and
driver, and `tally-campaign-poll.timer` once, unconditionally. The NixOS module
deploys the daemon only until
[`services.tally.campaignForge.enable`](../configuration/nixos-options.md#servicestallycampaignforgeenable)
is set; see [Campaigns on a NixOS host](#campaigns-on-a-nixos-host) for what
that switch renders and for the forge identity it requires. Declared
`services.tally.campaigns` remain Home Manager only on both paths.

The timer only scans locally armed
issue locators; an empty registry performs no work. A pass that advanced admits
its own successor through the events directory, so the timer is the recovery
path for a lost continuation event and the way an outside change to the issue
graph is noticed, not the ordinary way a campaign reaches its next pass. It
scans every `services.tally.campaignPoll.interval` (60s by default). A scan that
finds nothing moved costs three REST reads per armed campaign — the
authenticated actor, the master issue, and its sub-issue list — and no GraphQL
at all: it compares the master and sub-issue timestamps that fetch already
returned before it decides whether to run the bounded GraphQL steering walk or
the paginated master-comment read, so an idle campaign no longer pays for a
full sub-issue traversal every tick. A scan holds the registry
lock exclusively across its forge
round-trips, which blocks an interactive `tally campaign arm`, `disarm`, or
`list` for its duration; `services.tally.campaignPoll.timeout` caps that hold.
The GitHub CLI identity used
by the user service must be able to read the campaign issue graph and, for a
GitHub target repository, push, open, and merge pull requests.

An operator may author the master and sub-issues directly, but projection avoids
that hand-maintained copy. Start from one JSON worklist:

```json
{
  "schemaVersion": 1,
  "campaign": {
    "name": "crm-night",
    "repository": {
      "checkout": "/srv/spec-repositories/crm",
      "baseBranch": "main",
      "remote": "origin",
      "forge": "github"
    },
    "maxTasks": 32,
    "maxParallel": 3,
    "runtimeMaxSec": 86400,
    "pool": "campaign",
    "agent": {
      "adapter": "codex",
      "argv": [
        "Read the file whose path is in the TALLY_BRIEF environment variable and execute the mission it contains. That brief is your complete instruction set."
      ],
      "priority": "low",
      "runtimeMaxSec": 14400,
      "approvalPolicy": "never",
      "sandboxPolicy": "danger-full-access",
      "diagnosisSandboxPolicy": "read-only"
    },
    "gates": [
      {
        "kind": "command",
        "id": "fmt",
        "preflightArgv": ["nix", "develop", "--command", "sh", "-euc", "command -v rustfmt >/dev/null; printf '' | rustfmt --emit stdout >/dev/null"],
        "argv": ["nix", "develop", "--command", "cargo", "fmt", "--all", "--check"],
        "runtimeMaxSec": 900
      },
      {
        "kind": "command",
        "id": "no-stubs",
        "preflightArgv": ["sh", "-euc", "test -d crates; command -v grep >/dev/null"],
        "argv": ["sh", "-euc", "! grep -R -n -E 'todo!|unimplemented!|TODO' crates/"],
        "runtimeMaxSec": 900
      },
      {
        "kind": "command",
        "id": "deny",
        "preflightArgv": ["nix", "develop", "--command", "sh", "-euc", "command -v cargo-deny >/dev/null; test -x test/cargo-deny.sh"],
        "argv": ["nix", "develop", "--command", "test/cargo-deny.sh"],
        "runtimeMaxSec": 900
      },
      {
        "kind": "command",
        "id": "tests",
        "preflightArgv": ["nix", "develop", "--command", "sh", "-euc", "command -v cargo >/dev/null; command -v cc >/dev/null; cargo metadata --offline --format-version 1 >/dev/null"],
        "argv": ["nix", "develop", "--command", "cargo", "test", "--workspace"],
        "runtimeMaxSec": 900
      },
      {
        "kind": "command",
        "id": "clippy",
        "preflightArgv": ["nix", "develop", "--command", "sh", "-euc", "command -v cargo >/dev/null; command -v clippy-driver >/dev/null; cargo metadata --offline --format-version 1 >/dev/null"],
        "argv": ["nix", "develop", "--command", "cargo", "clippy", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"],
        "runtimeMaxSec": 900
      },
      {
        "kind": "command",
        "id": "flake-check",
        "preflightArgv": ["sh", "-euc", "export XDG_CACHE_HOME=/tmp/nix-cache XDG_STATE_HOME=/tmp/nix-state; mkdir -p \"$XDG_CACHE_HOME\" \"$XDG_STATE_HOME\"; exec nix flake metadata --no-write-lock-file >/dev/null"],
        "argv": ["sh", "-euc", "export XDG_CACHE_HOME=/tmp/nix-cache XDG_STATE_HOME=/tmp/nix-state; mkdir -p \"$XDG_CACHE_HOME\" \"$XDG_STATE_HOME\"; exec nix flake check -L"],
        "runtimeMaxSec": 900
      }
    ]
  },
  "tasks": [
    {
      "id": "customer-model",
      "kind": "implementation",
      "title": "Implement the customer model",
      "body": "## Goal\n\nImplement the frozen customer contract.\n\n## Acceptance\n\n- The focused model tests pass.",
      "dependencies": [],
      "conflictDomains": ["src/domain/customer.rs"]
    }
  ]
}
```

Project and arm it:

```console
$ tally campaign project ./crm-night.json --repo mecattaf/crm
{"issue":"https://github.com/mecattaf/crm/issues/42",...}
$ tally campaign arm https://github.com/mecattaf/crm/issues/42
```

`project` creates or maintains one labeled master issue, one native sub-issue
per task, and native blocked-by relations for `dependencies`. Re-run it with
`--issue URL` to update the same graph. It preserves prose outside tally's
marker-delimited manifest and worklist sections. A task may supply an existing
positive `issue` number; otherwise projection creates one. `--campaign-config`
accepts the campaign object in a separate file.

The embedded manifest owns configuration and typed task references. An
`implementation` reference carries `id`, `kind`, issue number, dependencies,
and, when declared, `conflictDomains`. A `checkpoint` reference carries `id`,
`kind`, issue number, dependencies, direct `argv`, and a positive
`runtimeMaxSec`; it never carries `conflictDomains`. Unsupported kinds fail
closed rather than becoming implementation work. The full human brief or
checkpoint description is always the sub-issue body. The manifest task set must
exactly equal GitHub's native
sub-issue set, task IDs form a topological order, and forge-native campaigns are
bounded to 100 tasks by the native sub-issue API. Drift fails closed before
admission. Directly authored sub-issues need no tally marker: their native
parent relation plus the manifest's issue number bind each brief to its task.
The task-body marker is projection ownership metadata added only by `project`;
an explicit `issue` field lets `project` adopt an unmarked task issue.

`campaign arm` is the sole normalization authority for that executable graph.
It resolves `repository.checkout` with the filesystem, requires the resolved
target to be a Git worktree, and stores the canonical path in the admitted
manifest before validation or hashing. A symlink or absolute path containing
`..` therefore pins its current target; retargeting the symlink later does not
silently redirect an armed campaign. An explicit steward containing only
`adapter` and `argv` is expanded to `env: {}`,
`finalMessagePattern: "^TALLY_FINAL_MESSAGE=(.*)$"`, and `runtimeMaxSec: 120`;
omitting the whole steward still means no steward.

That Rust admission path is also the only manifest grammar. Every manifest
object is closed, including both gate variants; forbid-path patterns may not
end in `/`; and the agent model, steward argv, environment, and scalar bounds
are enforced before `project` writes forge state or `arm` writes a registration
or enqueues a pass. `finalMessagePattern` may be omitted to select the default,
but it may not be `null`. An explicit pattern is limited to portable literals,
anchors, character classes, escapes, quantifiers, capture/non-capture groups,
and exactly one capture group. Backreferences, look-around, named or
conditional groups, and inline engine flags are rejected. This keeps the
admitted expression identical in Rust and Python rather than depending on an
engine-specific extension.

Rust hashes compact, recursively key-sorted UTF-8 JSON of the canonical
`{manifest,tasks}` object, then carries that same manifest, immutable task
content, and digest in a `CanonicalCampaignGraphV1` envelope on every flow
dispatch. Poll refetches and normalizes the live graph before it compares the
armed digest. The packaged Python driver consumes the carried envelope for
execution and fetches only mutable forge state; it does not resolve paths,
apply manifest defaults, or rebuild a second digest contract from the issue
body. Dispatch also retains the normalized `armedManifest` from the #433
receipt boundary. A normal flow carries both values. A direct compatibility
brief may omit `campaignGraph`: the driver exact-decodes `armedManifest`, fills
only the immutable title/body members from the native task issues, and requires
the reconstructed envelope to reproduce `worklist.graphDigest` before it can
execute. It applies no defaults and rewrites no paths. Python regex compile
failure remains an internal contract violation, because user-level regex
admission has already happened in Rust.

### Where the campaign contract's single source lives

`crates/tally-core/src/campaign_contract.rs` is the generative source for the
canonical manifest and graph contract. Its corpus test emits accepted canonical
manifests and graphs, their exact JSON bytes and digests, and named rejection
mutations, then byte-compares that generated document with
`test/fixtures/spec-build/contract-corpus.json`. The packaged Python driver's
exact decoders and `test/spec_build_contract_corpus_test.py` consume that same
file. Adding or removing a Rust manifest field therefore requires regenerating
the corpus and aligning the decoder before either language's checks are green;
the corpus also pins the required keys used by the two-repository regression
fixtures that exposed the #471 drift.

Two known peer copies remain hand-maintained and are not generated by this
corpus: the `campaignGraph` schema in `examples/flows/spec-build.js` and the
campaign schema and assertions in `nix/modules/common.nix`.

The master worklist is generated from those references:

```markdown
- [ ] <!-- tally:campaign-task:v1 id=customer-model --> #43 — Implement the customer model
```

Those boxes are a projection, not mutable truth, and which projection you get
depends on the arm-time capability probe below. Where GitHub serves the native
sub-issue walk, the parent's own sub-issue progress bar is the projection and
tally writes nothing: no checkbox edits, and no per-merge progress comment.
Where it does not, the driver recomputes the boxes at every reconcile from
revision-bound merged pull requests and content-bound checkpoint refs, and a
merge repairs its own box. Under either projection, closing a task issue or
manually checking a box cannot complete a task. A GitHub task PR includes
`Closes #<sub-issue>`. The authoritative completion-identity contract is
`task_completion_revision` in `crates/tally-core/src/campaign_contract.rs`;
the Python driver mirrors it. Its revision-bearing members are
`contractVersion`, `campaign`, `repository`, `mergeMethod`, `gitAiBinding`,
`gitAiAwaitSec`, `agent`, `steward`, `gates`, `task`, and `content`. `task`
contains `id`, `kind`, `issue`, `dependencies`, and `conflictDomains`; a
checkpoint task also contains `argv` and `runtimeMaxSec`. `content` contains
`number`, `title`, and `body`. Editing any of those inputs cannot reuse the old
PR as proof.

The contract deliberately excludes `maxTasks`, `maxParallel`, the
campaign-level `runtimeMaxSec`, `pool`, all scheduler fields, and all sibling
tasks. Capacity and scheduling decide when and where admitted work runs, while
sibling fields describe other work; none changes this task's admitted content
or the validation policy its completion proved. Those edits therefore leave
existing task revisions intact, so one graph edit invalidates only the affected
completion proofs instead of redispatching the whole campaign. This is the
shipped identity on both sides of the Rust/Python admission parity boundary,
not an aspirational projection rule.

### The sub-issue walk, and what a closed sub-issue does not prove

`arm` probes once, before it registers anything, whether this forge can serve
the walk the native read path needs: parent → `subIssues` →
`closedByPullRequestsReferences` → `pullRequest.merged`, in one bounded
GraphQL query per pass, paginated within the 100-task cap. A forge whose schema
has no such field is a capability answer, not a campaign failure — the campaign
arms in degraded mode and every projection above falls back to checkboxes. Only
a schema refusal counts: a transport error, a rate limit, or a 502 says nothing
about the forge and fails the arm loudly instead, because degrading on one bad
minute would silently cost the campaign its per-task steering threads, its
merged-oracle walk, and its anomaly surface for the rest of its life. A refusal
is read from the response's own typed `errors[]` entry, or — only on a call that
failed — from `errors[].message` and `gh`'s stderr. It is never read from the
response body: that body carries every comment on every task thread, and a
comment is writable by any account, so scanning it would let a stranger answer
the capability gate. Every `arm`
path reports which mode it recorded, as `subIssueWalk` and `projection`
(`native-sub-issues` or `degraded-checkboxes`) alongside its ordinary output;
`tally campaign list` shows the same field for an already-armed campaign. Read
it: a degraded campaign is otherwise indistinguishable from a native one until
an operator's comment on a task sub-issue silently fails to reach its agent.
The probe answer is recorded once and never revisited, so re-arm to re-probe.

The walk narrows **where** completion candidates come from; it never widens
**what** counts as proof. A pull request reached through a task's sub-issue
still completes that task only if its body carries the exact current
revision-bound marker and it passes the same code repository, base branch,
stable head branch, merge-commit and ancestry validation as before. It must not
narrow proof either: a sub-issue that links more closing pull requests than one
page returns fails the pass outright rather than reading completion from a
truncated page whose newest reference — the likeliest current proof — is the one
dropped.

An older revision's pull request never completes the current revision. The
native walk may expose it as a separate **re-stamp source**, but only if the
ordinary completion validator accepts it at the revision it actually names:
the exact historical marker and its derived stable head must match, the pull
request must be merged into the configured base in the code repository, and
its full merge-commit oid must be contained in the witnessed base. This is the
same proof with a historical revision as input, reached through the narrower
task-sub-issue surface. The degraded branch-scoped lookup does not search for
older revisions, and an unmerged pull request, a wrong head or base, a foreign
repository, a missing merge commit, or a commit outside the witnessed base
admits no re-stamp.

The reconciler reports those sources under `restamps`, not `merged`. The task
therefore remains incomplete and its dependants remain blocked. Its lane then
mints one deterministic empty commit when fresh (or verifies that resumed lane
history remains content-empty) and rejoins the ordinary ownership, tree-delta,
configured-gate, publication, rebase, and merge path. It skips the task-thread
steering re-check, implementation agent, and steward narration; the template
supplies the marker pull request text and no `Assisted-by:` trailer is invented.
The pull request keeps the `[marker]` title prefix. Only a later reconcile that
reads that merged pull request back with the exact current marker completes the
task.

A sub-issue is human-clickable, so its closure carries no authority at all.
`pullRequest.merged` remains the only oracle. When the walk finds a sub-issue
closed **by hand** while its task holds no revision-valid merged pull request
(or, for a checkpoint, no completion ref), the task stays incomplete and in the
frontier, and the pass records a typed `closed-without-merged-proof` anomaly.
`tally query run RUN-UUID` prints those anomalies above the task board and puts
the run in `needs-attention`; they are not filed with the reconciler's
warnings, because a reader who misses one debugs the wrong surface.

A closure the campaign caused itself is not that signal and is never reported as
one. A task pull request carries `Closes #<sub-issue>`, so the campaign closes
its own sub-issue as it merges. Editing that task or global execution policy
may rotate its revision while the sub-issue stays closed; a valid historical
pull request then becomes the re-stamp source above, without becoming current
completion. The discriminator is the marker prefix, read revision-blind: a
sub-issue closed by a merged pull request carrying this campaign's marker at
any revision was closed by the campaign, and a hand closure has no such pull
request.

### Per-task steering threads

Where the walk is available, each task's sub-issue is that task's steering
thread. A machine diagnosis or machinery-retry receipt for task `T` is posted on
`T`'s sub-issue and read back from there, so `T`'s retry brief carries `T`'s
history and no other task's. A comment by an allowed actor on `T`'s sub-issue
reaches `T`'s agent as `steering.authorizedComments` and advances the
observation revision exactly like a master comment does. That read takes the
newest 100 comments on the thread; a thread that exceeds it logs a warning
naming the sub-issue, because an approved comment older than the window stops
reaching its task and nothing else would say so. The master issue stays
the campaign-wide channel: campaign-level human steering reaches every task, and
escalation and the closing summary are always posted there.

That pass-start snapshot is not the last word for an implementation attempt.
After its lane is prepared and immediately before its adapter is admitted, a
deterministic driver node re-reads that task's thread once. In native mode this
is the task sub-issue and uses the same newest-100 window; in degraded mode it
is the master thread and uses the historical paginated REST read. The re-check
uses the exact `allowedActors` recorded at arm and applies the same rule that
excludes any comment containing the `<!-- tally:spec-build:` machine marker.
It folds newly authorized comments into the immutable agent brief; an
unauthorized or machine-marked late comment remains excluded. A read failure is
campaign machinery and prevents the adapter from being dispatched under a
possibly stale brief.

The immutable brief is also the attempt receipt. Its
`steering.attemptReceipt.preparedCommentIds` names comments already present in
the prep snapshot, while `lateRecheckCommentIds` names comments first observed
or updated by the pre-dispatch read. `rechecked`, `recheckTruncated`, and the
exact thread coordinate make the provenance explicit even when no new comment
arrived. The agent consumes the ordinary merged `authorizedComments` array and
does not fetch either public thread itself.

New receipts always go to the task's own thread, but the ledger reads both
surfaces and counts one receipt per `(kind, task, attempt)`. A campaign armed
before the walk capability existed — or re-armed into it mid-flight — has its
earlier receipts on the master, and discarding them reset every task's
diagnosis and retry counters, which bought one extra agent attempt and
re-posted a public comment that had already been made. The thread copy wins
where both surfaces carry the same attempt, since that is where the current
receipt lives; the duplicate is reported as a warning rather than counted
twice.

`arm` authenticates the current `gh` login, defaults the local allowlist to that
login, and requires the master and every task issue to have an allowed author.
Use repeatable `--allow-actor LOGIN` only after reviewing another actor's input;
the authenticated login is always included. It also binds a GitHub campaign to
the checkout's named `github.com` remote, validates configured pools and
adapters, agent policy names, flow fanout bound, and packaged assets, and records
the canonical executable graph SHA-256 beneath
`$XDG_STATE_HOME/tally/campaigns/armed/`. GitHub does not expose a trustworthy
body-editor identity, so the digest—not an inferred editor—is the continuing
authority boundary.

The JSON files in `campaigns/armed/` are closed schema-2 authority records. That
shape is frozen to what the preceding tally generation can read. Host-local
tuning is stored separately under
`$XDG_STATE_HOME/tally/campaigns/host-tuning/<registration-id>.host-v1.json`, so
an older binary scans only authority it understands. An explicit override
creates that sidecar; when it is absent, a current reader supplies the same
historical 10-second default without rewriting authority or creating a sidecar.
A current reader also repairs the short-lived schema-2 shape that incorrectly
embedded `projectionWaitMs`: while holding the registry lock it moves an
explicit value to the sidecar, treats `null` as the absent/default
representation, and rewrites the authority record. No other unknown authority
member is admitted by that migration.

The canonical graph used to interpret a later amendment is likewise outside
the frozen authority object. Each successful arm writes a closed schema-1,
digest-verified snapshot under
`$XDG_STATE_HOME/tally/campaigns/approved-graphs/<issue-scope>/<arm-serial>.graph-v1.json`
before publishing the matching authority generation. The path is generation
scoped, so an interruption can leave an unreferenced future snapshot but can
never make the current authority read a graph from another arm. A successful
publication prunes older graph generations; disarm and closed-issue pruning
remove the issue's snapshot directory. Missing snapshots on registrations from
an older tally remain readable and simply cannot prove a dependency addition.

Each successful arm also owns the exact flow and driver it admitted. Ownership
lives under
`$XDG_STATE_HOME/tally/campaigns/assets/<registration-id>/<arm-serial>/`, outside
the frozen authority schema:

- An asset below `/nix/store/<hash-name>/...` keeps its exact subfile path in
  authority, while `roots/flow` or `roots/driver` is a separate indirect GC root
  for the containing store output. Thus a profile upgrade and Nix garbage
  collection cannot collect the older campaign generation.
- Any other override is copied into `snapshots/flow` or `snapshots/driver` before
  authority is published. The registry removes write bits, preserves read and
  execute bits (including the driver's executable mode), records a SHA-256, and
  verifies both mode and content on later entries. The source may disappear
  after a successful arm; the immutable snapshot is the authority path.

There is no unmanaged asset-lifetime mode. Asset directories and both roots are
prepared and synced before the authority rename. Re-arm publishes the next
serial before removing the prior one; disarm and closed-issue pruning remove
authority before assets. Consequently, an interrupted pre-publication arm can
only leave an orphan generation, while an interruption after publication can
only retain an extra old generation. Registry entry recreates a missing root
while its output still exists and deletes only unreferenced registry-owned
generations. If a legacy unrooted path has already disappeared, `arm`, `list`,
and `poll` fail with a typed missing-asset error instead of silently substituting
the current package. Explicit `disarm` can still abandon that broken authority;
it removes authority first and reconciles every registration that remains live.

The poller may refetch projection state and allowed-actor comments, but it
refuses any changed executable graph until the operator inspects it and runs
`arm` again. The reconciler independently refetches and recomputes that digest
before executing. Checkbox/state/timestamp changes are observation signals, not
executable changes. Each implementation attempt receives a bounded authorized
steering snapshot: the pass-start value plus the one task-thread re-check
immediately before agent dispatch. Agents are told not to refetch the public
comment channel themselves. Re-arming the same unchanged
graph forces a fresh retry without invalidating matching completion facts;
re-arming changed task content creates a new revision for that task.
`--no-enqueue` registers after validation without starting a pass, and `--wait`
returns its terminal verdict. A graph mismatch is a structured poll result, not
a broken poller: `campaign poll --once` returns success with one `blocked` item
whose reason is `rearm-required` and which names both the approved and live
digests. Forge, registry, validation, and dispatch failures remain under
`failures` and return nonzero.

Re-arm also interprets one deliberately narrow amendment as structural
steering. Tally compares the newly admitted graph with the prior arm's
canonical graph. If a task is still escalated and this amendment added at least
one dependency to that same task, `arm` pardons only that task's diagnosis and
machinery-retry counters. Adding a dependency to a task that is not escalated,
or changing any other field of an escalated task, does not pardon anything.
Each pardon uses the ordinary marked resume receipt with a task scope, reason,
requesting actor, and stable GitHub comment URL. The arm JSON records it under
`autoPardons` with the task ID, added dependencies, and `resumeReceipt`.

An escalated task that gained no dependency remains untouched. The arm JSON's
`warnings` names it explicitly as `task X remains escalated; run tally campaign
resume to unblock`. One amendment may therefore pardon the tasks it addresses
while retaining the counters and shared escalation for every task it does not.
A registration created before canonical graph snapshots existed has no trusted
baseline for its first re-arm: tally cannot infer that a dependency was added,
so it pardons nothing and emits the same warning for each active escalation.

An escalated GitHub campaign has one non-destructive recovery verb:

```console
$ tally campaign resume https://github.com/OWNER/REPO/issues/42 \
    --reason "Corrected the unavailable external dependency" --wait
```

`resume` requires an existing armed registration and the same authenticated
actor, refetches and validates the live issue graph and host, posts a marked
receipt containing the reason, advances the arm generation, approves the live
graph, and dispatches a new pass. The receipt's stable GitHub comment id is the
ledger boundary: earlier diagnosis, retry, and escalation comments remain on
the issue as audit history but no longer spend the new generation's counters;
later receipts count from attempt 1. The authority record is written before
dispatch; once that write succeeds, the periodic poller can recover an
interrupted dispatch without deleting comments or hand-editing state. `resume`
is limited to `repository.forge=github`; the explicitly test-only local forge
has no GitHub comment boundary.

The public manual verb remains a campaign-wide pardon. An automatic re-arm
pardon uses the same marker, heading, reason, actor attribution, and stable
comment-ID boundary, with `tasks=...` added to the marker. Diagnosis and retry
receipts at or before that boundary are ignored only for the named tasks. A
campaign-wide escalation is pardoned only when those scoped boundaries cover
every task whose two diagnosis receipts produced it; otherwise the escalation
stays live for the unaddressed task. Scoped receipts therefore preserve the
same append-only audit history without turning one amended task into a silent
global budget reset.

The default ad-hoc `workspaceRoot` is
`$XDG_CACHE_HOME/tally/campaigns/workspaces` (or
`~/.cache/tally/campaigns/workspaces`), outside tally's state/data byte budgets.
Override it when those workload-owned lanes need a dedicated monitored volume.
`forge: "local"` is rejected unless `--allow-test-local-forge` is supplied; it
is a mechanism-test mode and has no autonomous continuation contract.

Merge/checkpoint projection changes and allowed steering comments advance the
observation revision, so the poller submits a fresh pass behind the capacity-one
campaign mutex. Both the cheap poll precondition and the full revision include
the remote base plus the driver's campaign-scoped state refs. A plain
`campaign poll --once` therefore sees a completed local merge or checkpoint and
admits its successor even when every forge timestamp is unchanged; the
flow-authored continuation merely schedules that same public scan. When every
task has durable proof, reconcile repairs all boxes, closes task issues, posts
one digest-bound closing summary, and closes the master. The next poll prunes
that registration.

A campaign has two terminal outcomes and both render the same closing summary:
completion, and escalation at frontier quiescence. The summary is markdown
rendered from a run-scoped digest — merged tasks with their pull requests,
checkpoints with the revision they bound, blocked tasks with what blocks them
and how many steered attempts they spent, tasks never attempted, every steering
note and machinery fault, anomalies, and reconciler warnings. Every field is a
projection of facts the pass already witnessed; the digest is not a state
store. On completion the summary is the campaign's single
`tally:campaign-complete:v1` comment. A forge-native campaign posts it and then
closes its task sub-issues and its master issue, so the digest is the last thing
a reader of the closed issue sees. That campaign thread remains GitHub-owned
when `--allow-test-local-forge` selects local Git state for code merges; the
merge backend never redirects the terminal comment into a local state ref. A
file-worklist campaign posts the same comment and stops: its master issue is a
projection whose lifecycle tally has never owned, so it stays open for whoever
does. At quiescence the summary is
published *before* the escalation, because the escalation is what every later
pass reads back to decide the campaign has already stopped — publishing it
second would mean one transient failure lost the digest for good, with no later
pass willing to retry.

Neither summary is ever an upsert — a summary the operator is not notified
about is not a summary — and both are idempotent: a repeated terminal pass
finds its own marker and stays quiet. Both render inside nodes the campaign
already had, so neither adds a flow node. For a non-native campaign whose issue
coordinate uses the local forge, the summary is a durable blob at
`refs/tally/spec-build/v1/<scope>/summary/<outcome>`. Operators can remove an
open registration without changing forge state with
`tally campaign disarm ISSUE-URL`; registry
read/modify/write and asset ownership operations are file-locked against timer
and re-arm races. Disarm removes that registration's sidecar, snapshots, and
both indirect roots after removing its authority record.
`tally campaign list` inspects registrations, while `tally campaign poll --once`
is the timer's bounded scan and `--wait` waits for newly admitted passes.

The opt-in `test/campaign-github-e2e.sh` receipt starts the final-package daemon
on a private temporary socket and exercises that exact package against a real
private GitHub repository: two dependent agent tasks, real PR creation and
merge, revision markers, checkbox repair, next-pass polling, closeout, and
registration pruning. It requires `TALLY_CAMPAIGN_E2E_CONFIRM=1` and attempts
to delete only the repository it creates; when the GitHub token lacks deletion
scope it archives the repository instead. Set `TALLY_CAMPAIGN_E2E_KEEP_REPO=1`
to retain it for inspection. Set `TALLY_CAMPAIGN_E2E_DAEMON_MODE=host` to
exercise an already deployed daemon whose protocol generation matches the
package.

## Campaigns on a NixOS host

The single-operator deployment is Home Manager, and nothing below changes that.
A host with no user session — a builder, a server, a machine nobody logs into —
runs the system daemon instead, and that daemon can execute forge-native
campaigns once two things are true: the campaign surface is rendered, and the
service account has a forge identity.

```nix
services.tally = {
  enable = true;

  campaignForge = {
    enable = true;
    login = "tally-bot";
    # A path, never a secret. Activation reads the file; the module never does,
    # and nothing about the token enters the Nix store.
    tokenFile = "/run/secrets/tally-campaign-forge-token";
  };
};
```

`campaignForge.enable` is the whole execution surface in one switch. It renders
the generic campaign pools (`campaign`, `campaign-agent`, `campaign-control`,
and `flow`), the packaged spec-build driver adapter, the fanout floor, and the
`campaign-continuation` events-directory registry entry — exactly the surface
`tally campaign arm` validates a host against before it spends agent time — and
then installs `tally-campaign-poll.service` and its timer as system units owned
by the service account. With the switch off, the module renders none of it, as
before: a poll timer without pools, driver adapter, or identity would fire on
schedule and fail every tick, which is worse than absent because it looks like a
broken campaign rather than an unsupported one.

Declared `services.tally.campaigns` stay Home Manager only, and the NixOS
assertion still refuses them. A declared campaign is driven by a managed GitHub
mention producer, and this module renders no producer units at all. The one
registry entry it does carry, `campaign-continuation`, renders no unit anywhere:
`tally-drain.timer` already drains that directory at the same cadence.

### The identity

This is the substantive difference between the two modules. A Home Manager
campaign runs as the operator and inherits the operator's own authenticated
`gh` and `git`. The system service account has neither, and every campaign job
runs as that account: the driver opens and merges pull requests, the agent
writes commits, the poll scan reads the issue graph. Campaign jobs are transient
units in the account's own user manager, so a unit environment on the poll
service would not reach them, and the shipped driver reads no `LoadCredential`.
The account's home is therefore the identity, and activation materialises it:

- The account's home moves from `/var/empty` to
  `services.tally.campaignForge.homeDir` (`/var/lib/tally/forge` by default),
  which is also where the campaign CLI's own XDG fallbacks resolve.
- `~/.config/gh/hosts.yml`, mode `0600`, holds the declared login and the token
  read from `tokenFile`. The token is piped to the identity writer on standard
  input, so it is never a program argument and never a store path; the file it
  comes from is read by root, so it needs no particular ownership.
- `~/.gitconfig`, mode `0600`, binds the commit identity
  (`campaignForge.gitUserName` and `gitUserEmail`, defaulting to the login and
  its GitHub no-reply address) and a `gh auth git-credential` helper, so https
  pushes authenticate with that same one copy of the token.
- `~/.config/gh/config.yml`, mode `0600`, holding nothing but `version: "1"`.
  This is the file `gh` writes for itself on first use, and it fails the whole
  call when it cannot (`failed to write config after migration`). Writing it at
  activation is what makes the home **read-only-safe afterwards**: every
  consumer — the driver adapter, the `shell` adapter that runs a campaign's own
  self-continuation, the agent adapters — can read this identity without needing
  write access to it.

Activation writes a fourth file, `~/.tally-campaign-forge-identity`, which marks
the directory as this module's to manage.

Rotating the token is an ordinary activation: replace the file's contents and
rebuild. Turning `campaignForge.enable` back off is a teardown: the same
activation snippet removes the identity files it wrote — but only in a home
carrying that marker, so a `homeDir` pointed at a pre-existing home keeps its
own `.gitconfig`. Removing the tally module altogether leaves the directory
behind, because there is no activation left to run; delete `homeDir` by hand and
revoke the token when a host is retired.

The poll service and the driver adapter still declare the home writable, which
costs nothing and keeps a hand-edited or half-provisioned home self-healing.
After a successful activation nothing requires it.

If the token is provisioned by sops-nix or agenix, this snippet is ordered after
`setupSecrets` and `agenixInstall` when the estate runs one — as a declared
dependency, not by luck of the name. A token file that is missing or unreadable
at activation fails that snippet with a message naming
`services.tally.campaignForge.tokenFile`, and the poll service refuses to start
until the identity exists, so an unprovisioned host stays quiet rather than
failing a unit on every tick.

One first-deployment caveat: campaign jobs inherit `HOME` from the service
account's user manager, and that manager reads the account record when it
starts. On a host where the manager is already running — anything but a fresh
boot — restart it once after the first activation that sets this option, so jobs
see the new home rather than `/var/empty`:

```console
# systemctl restart user@"$(id -u tally)".service
```

The token needs the scopes a campaign actually uses: read the issue graph, push
branches, open pull requests, and merge them. The campaign's checkout must be
writable by the service account and must have an https `github.com` remote.

### Arming from the system host

The registry lives under the system state directory, which is mode `0700` and
owned by the service account, so `arm`, `disarm`, and `list` run as that
account and must name the same state directory the poll unit scans. Passing a
different one — or omitting `--state-dir`, which resolves through `HOME` — files
a registration no timer will ever read.

`--allow-actor` is effectively mandatory here, and this is the one command that
differs materially from the Home Manager path. `arm` defaults the allowlist to
the account it authenticates as and then refuses a master or task issue authored
by anyone outside it. On Home Manager that account is the operator, who also
wrote the issues; here it is the bot, so the human who filed them has to be named
— once, at arm time, which is the review boundary the flag exists for.

```console
# runuser -u tally -- tally \
    --config /etc/tally/config.json --socket /run/tally/tally.sock \
    campaign arm --state-dir /var/lib/tally/state \
    --allow-actor OPERATOR-LOGIN ISSUE-URL
# runuser -u tally -- tally \
    --config /etc/tally/config.json --socket /run/tally/tally.sock \
    campaign list --state-dir /var/lib/tally/state
```

Those two global locators become the prefix of every child the campaign host
creates, not just the interactive arm: both the initial `tally flow run` and
the machine continuation start with
`tally --config /etc/tally/config.json --socket /run/tally/tally.sock ...`.
The flags remain before the `flow` or `campaign` subcommand. This matters on a
system host because `/etc/tally/config.json` is the only rendered tally config;
the service account's XDG home intentionally carries forge identity, not a
second config copy. A manually started host that did not select `--config`
continues to omit the pair rather than serializing a guessed XDG path.

The alternative is to have the bot author the issue graph in the first place, by
running `campaign project` under the same `runuser`; then the default allowlist
already matches and no flag is needed.

`services.tally.campaignPoll.interval` and `.timeout` mean what they mean on
Home Manager; on this module they take effect only while `campaignForge.enable`
is set.

## Configure a recurring campaign

Declared campaigns are a Home Manager surface because their GitHub producer is a
managed user service. The configured checkout must be writable by that user,
have the named remote, and already have Git and `gh` authentication suitable for
pushing, opening pull requests, and merging them.

```nix
services.tally = {
  enable = true;

  campaigns.crm = {
    enable = true;

    repositories."mecattaf/crm" = {
      checkout = "/srv/spec-repositories/crm";
      baseBranch = "main";
      remote = "origin";
    };

    label = "spec-build";
    # Substitute your own GitHub login. This token is posted as a real comment
    # on a real issue, so it at-mentions whoever it names.
    mention = "@<your-login> build";
    # The operator posts the mention using this same account's gh token.
    allowSelfTriggered = true;
    allowedActors = [ "mecattaf" ];
    # Public failure comments and stderr publication are independently
    # default-off. Diagnose locally unless this campaign explicitly needs them.
    postFailureEvidence = false;
    postFailureStderr = false;
    worklist = "specs/crm/tasks.json";
    maxTasks = 32;
    maxParallel = 4;

    agent = "codex";
    # These are the defaults. Keep them explicit here to show that the
    # implementation node can reach git metadata and asks nobody for
    # permission, and that the diagnosis node cannot write at all.
    agentApprovalPolicy = "never";
    agentSandboxPolicy = "danger-full-access";
    agentDiagnosisSandboxPolicy = "read-only";
    gates = [
      {
        kind = "forbidPaths";
        id = "no-db-artifacts";
        forbidPaths = [ "*.db" "*.db-wal" "*.db-shm" "*.sqlite*" ];
      }
      # Each probe exercises the estate dependency its own gate would die on:
      # the development shell activating, the compiler driver the linker step
      # needs, and the workspace manifest resolving offline. A bare
      # `--version` call proves none of that and is the probe the pristine-base
      # preflight exists to replace.
      {
        kind = "command";
        id = "fmt";
        preflightArgv = [
          "nix" "develop" "--command" "sh" "-euc"
          "command -v rustfmt >/dev/null; printf '' | rustfmt --emit stdout >/dev/null"
        ];
        argv = [ "nix" "develop" "--command" "cargo" "fmt" "--all" "--check" ];
        runtimeMaxSec = 900;
      }
      {
        kind = "command";
        id = "no-stubs";
        preflightArgv = [ "sh" "-euc" "test -d crates; command -v grep >/dev/null" ];
        argv = [ "sh" "-euc" "! grep -R -n -E 'todo!|unimplemented!|TODO' crates/" ];
        runtimeMaxSec = 900;
      }
      {
        kind = "command";
        id = "deny";
        preflightArgv = [
          "nix" "develop" "--command" "sh" "-euc"
          "command -v cargo-deny >/dev/null; test -x test/cargo-deny.sh"
        ];
        argv = [ "nix" "develop" "--command" "test/cargo-deny.sh" ];
        runtimeMaxSec = 900;
      }
      {
        kind = "command";
        id = "tests";
        preflightArgv = [
          "nix" "develop" "--command" "sh" "-euc"
          "command -v cargo >/dev/null; command -v cc >/dev/null; cargo metadata --offline --format-version 1 >/dev/null"
        ];
        argv = [ "nix" "develop" "--command" "cargo" "test" "--workspace" ];
        runtimeMaxSec = 900;
      }
      {
        kind = "command";
        id = "clippy";
        preflightArgv = [
          "nix" "develop" "--command" "sh" "-euc"
          "command -v cargo >/dev/null; command -v clippy-driver >/dev/null; cargo metadata --offline --format-version 1 >/dev/null"
        ];
        argv = [
          "nix" "develop" "--command" "cargo" "clippy" "--workspace"
          "--all-targets" "--all-features" "--" "-D" "warnings"
        ];
        runtimeMaxSec = 900;
      }
      {
        kind = "command";
        id = "flake-check";
        preflightArgv = [
          "sh" "-euc"
          "export XDG_CACHE_HOME=/tmp/nix-cache XDG_STATE_HOME=/tmp/nix-state; mkdir -p \"$XDG_CACHE_HOME\" \"$XDG_STATE_HOME\"; exec nix flake metadata --no-write-lock-file >/dev/null"
        ];
        argv = [
          "sh" "-euc"
          "export XDG_CACHE_HOME=/tmp/nix-cache XDG_STATE_HOME=/tmp/nix-state; mkdir -p \"$XDG_CACHE_HOME\" \"$XDG_STATE_HOME\"; exec nix flake check -L"
        ];
        runtimeMaxSec = 900;
      }
    ];

    # Held for one bounded reconcile pass, so two triggers cannot mutate this
    # campaign concurrently.
    pool.name = "crm-campaign";
  };
};
```

### The mention token is a live GitHub mention

`mention` is matched literally against the body of a comment, so tally treats it
as an opaque token — but the comment it matches is a real comment on a real
issue, and GitHub resolves every `@name` in it. **Name your own login and never
a third party's.** A campaign that ships `@someone-else build` notifies that
account on every trigger, forever, from a repository they have nothing to do
with; the shipped `services.tally.campaigns.<name>.mention` default is exactly
that shape and must be overridden. A token naming a login that does not exist
notifies nobody, and a token with no `@` at all is a perfectly good trigger
grammar — nothing about the mechanism requires the mention form.

Naming your own login is also the coherent choice, because it composes with the
two authorization switches directly below: the account you at-mention is
normally the account posting the comment, which is either the authenticated
`gh` identity (`allowSelfTriggered = true`, as in the block above) or a login in
`allowedActors`. Under a bot identity, name the bot.

`allowSelfTriggered` defaults to `false` on the operator-facing mention
producer. Keep that loop-breaker when tally's authenticated GitHub identity is
a bot. Set it to `true` only when the trusted person posting the campaign mention
is also the account authenticated by `gh`, as in the single-account example
above. `allowedActors` filters external actors on this producer;
`allowSelfTriggered` is the separate authorization for the authenticated `gh`
identity and therefore does not require adding that identity to the external
allowlist.

Neither switch governs the machine's own next-pass nudge, and that is the
point: a campaign no longer continues itself through this producer at all. A
pass that advanced drops a JSON enqueue payload into the daemon's events
directory, which the shipped drain admits — no comment is posted, no forge round
trip is on the critical path, and nothing about external campaign admission is
widened. Advancing means any of five things: a task merged, a checkpoint passed,
machine steering was published, a machinery retry was posted, or a checkpoint
deferred. The last two matter most, because they are the passes that produced no
completion at all: a lane that only faulted, or a checkpoint that only deferred,
still has to reach its next attempt. The single
`producers.campaign-continuation` events-dir entry described below is that
mechanism; the mention producer these two switches configure remains the human,
remote trigger surface.

`postFailureEvidence` posts one comment for each failed attempt, so retries can
accumulate several receipts. `postFailureStderr` requires it and adds only the
bounded, conservatively redacted tail. The receipt states how much redaction
removed: `stderrRedacted` says whether anything was dropped and
`stderrRedactions` counts the replacements *in the published tail*, so one
dropped token reads differently from forty dropped lines. When
`stderrTruncated` is also true the head of the tail was dropped for length, and
any redaction that fell in the dropped head is not counted — the number always
describes the text in front of you. Redaction cannot recognize every
application secret; leave both defaults off for a public repository unless the
publication policy has been deliberately reviewed. These settings belong to the
campaign's mention producer, which is now the only producer a campaign renders;
the continuation carries no publication policy because it publishes nothing.

Checkpoint output retention is separate from publication. Every checkpoint
attempt, green or red, unconditionally snapshots the final 8 KiB of stdout and
the final 8 KiB of raw stderr into
`<stateDir>/capture/archive/<taskUuid>.<taskId>/checkpoint.json`. A retry or
steering receipt names that local path, and the terminal escalation collects
the checkpoint paths from its accumulated receipts. The retry includes roughly
the last ten stderr lines only when both `postFailureEvidence` and
`postFailureStderr` are enabled; those lines pass through the same
`conservative-v2` public redaction as other failure stderr. With either switch
off, the local path remains visible but no checkpoint output is copied into the
comment.

Every gate sets an `id`, an explicit `kind`, and the fields for that kind:
`kind = "command"` requires `preflightArgv` and `argv`, while `kind =
"forbidPaths"` requires `forbidPaths`. Gate commands are direct argv, not shell
strings. Use `sh -c` explicitly only when shell syntax is actually part of
project policy. `agentArgv` normally stays at its default: a fixed
instruction telling the adapter to read the structured brief at `TALLY_BRIEF`.
It can be overridden for a fixture or a purpose-built adapter executable, but
the campaign never interpolates task prose into argv. Command gates also run as
the accept-time preflight described below; history constraints begin after the
agent has produced committed task work.

### Direct argv under hardened adapters

Every checkpoint `argv` and command-gate `preflightArgv`/`argv` is a direct
argv array. The array must be non-empty, every element must be a non-empty
string, and no element may contain a control character. In particular, a shell
program passed to `sh -c` must remain one line: a literal newline or tab is
rejected at admission even though the shell itself could parse it.

Some otherwise valid argvs depend on writable caches below `$HOME`. Under an
adapter whose resolved tier sets `ProtectHome=read-only`, bare invocations of
`nix`, `go`, `cargo`, `npm`, `pip`, or `uv` can therefore fail before doing
useful work. Redirect cache and state in the same argv, using `XDG_CACHE_HOME`
and `XDG_STATE_HOME` or a tool-specific equivalent such as `GOCACHE`. For
example, the hardened-safe Nix argv creates its cache inside the job's private
`/tmp`:

```json
["sh", "-euc", "export XDG_CACHE_HOME=/tmp/nix-cache XDG_STATE_HOME=/tmp/nix-state; mkdir -p \"$XDG_CACHE_HOME\" \"$XDG_STATE_HOME\"; exec nix flake check -L"]
```

That private location must be created by the argv itself. With `PrivateTmp=yes`,
a `/tmp/input` staged by the caller before dispatch and `/tmp/input` seen by the
transient unit are different paths. Put staged inputs in the lane workspace or
another path writable and visible to the unit; reserve private `/tmp` for data
the unit creates itself. Likewise, do not direct output through `$HOME` when
`ProtectHome=read-only` may apply.

Hardening is resolved per adapter, and the adapter option defaults to `null`.
The shipped campaign adapters do not select a preset today, so the effective
tier is an estate choice: check the resolved adapter configuration on the host
instead of assuming `strict`. When a preset is selected, ordinary jobs use the
property bundle built by `push_hardening_properties`; its strict and production
tiers allow `AF_UNIX AF_INET AF_INET6`. That is deliberately broader than the
separate git-ai unit's UNIX-only address-family set.

`tally campaign arm` scans checkpoint and command-gate argvs for these cache,
private-`/tmp`, and `$HOME`-write shapes. The findings are named, heuristic
warnings in the arm receipt, never admission errors.

`forbidPaths` is evaluated against the union of committed, non-deleted paths
changed by every branch commit reachable from the current `HEAD` but not from
the task's prepared base revision. A later deletion does not erase an earlier
forbidden artifact from this history-scoped check. Deletions of artifacts that
were already tracked in the prepared base are ignored, so a task may remove
legacy debris. If a task commits a forbidden artifact, remediation must rewrite
or squash the task branch so the offending commit is no longer reachable;
adding a cleanup commit is intentionally still red.

A rejection reports `N path(s) touched in lane history` and says explicitly
that a later removal does not clear the failure; the check id and offending
paths remain literal in that detail. The diagnosing steward receives the same
rule in operational terms: the gate walks
`changed_paths_in_history(union_base, head)`, so the only cure is to rewrite the
lane until the path appears in no lane commit — for example, soft-reset to the
merge base and recommit without it.

Matching folds case over repository-relative POSIX paths, so `*.db` also rejects
`build/TRANSIENT.DB`. A pattern without `/` matches a basename at any depth. A
pattern with `/` is rooted at the repository; `*` and `?` stay within one path
component and `**` spans zero or more complete components. Because zero is
included, `build/**` also matches a tracked file literally named `build`.
`**` must be a complete component: write `src/**/*.db`; `src/**.db` is rejected
as ambiguous. Patterns are bounded, unique, relative, and may not contain `..`.

The constraint uses one Git history query and in-memory glob matching in the
packaged driver. It is still an ordinary `campaign-control` node with `exit:0`
evidence, its declared `runtimeMaxSec`, a stable `gate-<task>-<id>` key, and a
canonical witness. Its schema-validated result records the prepared base and
the exact checked head. Publication re-evaluates every constraint against the
clean head it is about to push, so a passed stable node cannot be reused for a
later unexamined commit. The rebase path applies the same pattern set to its
rewritten head before force-pushing it. A match therefore fails the node—or the
exact-head publication recheck—and stops publication exactly like a nonzero
argv gate; it is not an operator audit after the merge.

This mechanism constrains task branches advanced by this campaign. It does not
turn the same rule into a repository-wide GitHub branch protection for unrelated
pull requests.

### Recommended gate ladder

Declare the complete merge criterion before arming a campaign, in this
cheap-fails-first order: `fmt` → `no-stubs` → `deny` → `tests` → `clippy` →
`nix flake check -L`. The two examples above spell out that ladder as gate
objects. The final flake check is not redundant with the narrower Rust gates: it
also evaluates the Nix surface and runs the checks wired into the flake, so a
task that leaves that surface red must fail before merge rather than at a later
campaign checkpoint.

The measurements from 2026-08-10 were `fmt` 1 s, `deny` 1 s, `tests` 286 s,
`clippy` 28 s warm-inherited from the preceding Rust build, and
`nix flake check -L` about 100 s. The no-stubs scan was not separately timed.
All of these numbers assume both a warm Nix store and a warm Cargo registry;
they are ordering evidence, not cold-host runtime budgets.

Under the `strict` and `production` hardening tiers, give the flake gate writable
cache and state roots in its private `/tmp`. This is the hermetic argv, expressed
as the command it runs:

```console
sh -euc 'export XDG_CACHE_HOME=/tmp/nix-cache XDG_STATE_HOME=/tmp/nix-state; mkdir -p "$XDG_CACHE_HOME" "$XDG_STATE_HOME"; exec nix flake check -L'
```

A bare `nix flake check -L` dies in about 1 s under
`ProtectHome=read-only` when Nix tries to write
`$HOME/.cache/nix/fetcher-cache-v4.sqlite`; opening all of home for that cache
would weaken the tier instead of making the gate self-contained.

Do not add this ladder piecemeal after arm. `gates` is a revision-bearing member
of every task completion, so adding one gate to an armed campaign rotates every
completed task's revision and requires a full marker re-stamp walk. Re-stamping
has been agent-free since `c3b7ffc`, but it still opens one marker pull request
for every completed task. Adopt the complete ladder at arm time.

Repositories that require every content-bearing lane to update `CHANGELOG.md`
can use this marker-safe command-gate predicate as `argv`:

```
sh -euc 'base="$(git config --get tally.baserev)"; git diff --quiet "$base" HEAD -- && exit 0;
         git diff --name-only "$base" HEAD -- CHANGELOG.md | grep -qx CHANGELOG.md'
```

This exempts only an empty tree diff, which is what an agent-free completion
re-stamp produces; marker pull requests are identified on GitHub only by the
`[marker] ` title prefix, not by a label. A content-bearing lane passes only when
`CHANGELOG.md` appears in its diff.

The base cannot come from pull-request metadata. A command gate runs with the
lane worktree as its current directory and only `CAMPAIGN_TASK_ID` added to its
environment—there is no pull-request number or GitHub base SHA—so the predicate
reads the `tally.baserev` per-worktree Git config that the driver writes for both
task and preflight lanes. The empty-diff exemption cannot be manufactured by a
re-stamp: `action_restamp` refuses any lane history that changes repository
content, checks the resulting base-to-head diff again, and only then returns the
empty commit. The re-stamp lane also genuinely runs the configured gates; after
the driver creates that commit, the flow falls through to the same ownership,
gate, publication, rebase, and merge path as substantive work.

The predicate is **not post-rebase-safe yet**. `action_rebase` returns the new
base revision to the flow, and the flow passes that value in the regate workspace
object, but the driver does not rewrite the lane's `tally.baserev` worktree config
before command gates run again. A post-rebase invocation therefore still diffs
from the original prepared base. Treat this changelog gate as post-rebase-unsafe
pending a driver fix that updates the per-worktree key before regating.

The campaign runner follows the same rule. Its complete structured flow
arguments travel in the producer enqueue's content-addressed brief and are read
from `TALLY_BRIEF`, with `TALLY_BRIEF_HASH` binding the runner to the admitted
bytes; the runner argv contains only the pinned tally executable, flow script
path, and stable control flags. Repository maps, gate definitions, agent argv,
store paths, and other campaign policy therefore do not inflate job queries or
transient-unit status output. GitHub issue bodies are not campaign flow args:
they remain in the separate `TALLY_GH_CONTEXT` file before and after this
transport for recurring campaigns.

An implementation node defaults to `agentSandboxPolicy = "danger-full-access"`
because its contract requires a commit and a merely writable sandbox is not
enough to produce one: under codex's `workspace-write` the repository's git
metadata is mounted read-only, so the agent writes every file correctly and then
fails at `.git/index.lock` — all of the work done, none of it publishable. An
adapter states which of its sandbox policies can commit in
`launch.commitCapableSandboxPolicies`; naming any other policy for an
implementation node is refused at evaluation time and again when the campaign is
armed, rather than three seconds into the first node. Prove the pairing against
the real binary with `tally adapter smoke <adapter> --sandbox <policy>
--assert-commit` before deploying it; that probe works under every `hardening`
preset, and `--probe-root` points it at the campaign's own workspace root.

The default `agentApprovalPolicy = "never"` follows from the same unattendedness:
a campaign node runs with nobody present to grant an escalation, so asking for
one can only stall. A diagnosis node's brief prohibits mutation, so it does not
inherit the implementation node's policy: `agentDiagnosisSandboxPolicy` defaults
to `"read-only"` and holds the node to its stated obligation, and commit
capability is not required of it. Every configured name must exist in the
selected adapter's launch maps; deployment fails early otherwise. Set any of
these options to `null` only for an adapter such as the shell fixture that
declares no corresponding policy map.

One enabled attrset expands to all of the following:

| Rendered mechanism | Contract |
|---|---|
| `flows.<name>` | The content-addressed shipped `spec-build` script, bounded to one `maxParallel` frontier and its gates. |
| `producers.campaign-<name>` | A GitHub search producer scoped to the configured repositories, open issues, label, exact mention, and optional actor allowlist. |
| `<pool.name>` | A capacity-1 mutex held for one reconcile pass. |
| `campaign-agent` | A counted `slot` pool with baseline capacity four, raised when an enabled recurring campaign has a larger `maxParallel`. |
| `campaign-control` | A `cpu-slot` pool for reconciliation, Git, GitHub, and gate nodes, with the same baseline and recurring-campaign scaling. |
| `spec-build-driver` | The packaged deterministic policy driver used for reconcile, prep, ownership checks, built-in constraints, checkpoint recording, diff capture, steering, machinery retries, escalation, continuation, publish, rebase, and merge projections. |

One further mechanism is installed once for the whole host rather than per
campaign: `producers.campaign-continuation`, an `events-dir` registry entry
declaring the contract that the machine self-continuation every campaign class
writes — after any pass that advanced — is an events-directory enqueue payload.
It is not per-campaign state, so arming still requires no Nix change.

That entry renders no unit of its own: it carries `selfDrain = false`, and the
shipped `tally-drain.timer` is the single drainer. Every tally home already ran
that timer unconditionally over the same directory at the same five-second
cadence, and the drain RPC claims the whole directory whoever calls it — the
`producer` parameter only stamps the durable admission origin. A second timer
therefore bought no coverage: it added one systemd unit and one call per
interval on every host whether or not it runs campaigns, and made the
`origin.producer` recorded for a campaign's own self-continuation depend on
which of the two timers won the race. `campaigns.<name>` refuses the reserved
name `continuation` for the same reason: `campaigns.continuation` would render
`producers.campaign-continuation` as a `gh` producer and replace the entry.

The name is also why the campaign layer declares `${stateDir}/events` and
`${stateDir}/capture/archive` in the `spec-build-driver` adapter's
`extraWritablePaths`. The continue node writes the former and checkpoint
recording writes the latter. Those are hard write dependencies the
compatibility default (no hardening preset) leaves unconstrained but `strict`
or `production` would otherwise refuse.

The producer posts its receipt and witnessed evidence. Under the degraded
projection, each merge and passed checkpoint repairs its own worklist checkbox
and a passed checkpoint posts an idempotently marked progress comment; under
the native sub-issue projection neither is written. Once task execution,
integration, and diagnosis settle, a pass that merged a task, passed a
checkpoint, published machine steering, posted a machinery retry, or deferred a
checkpoint writes its continuation payload from one separate
node. That node writes no comment: the file lands in the events directory under
a name and `dedupKey` derived from this pass's identity, and the drain admits a
fresh pass behind the campaign mutex. Neither the producer nor the drain closes
the campaign issue. It remains the durable steering and scheduler-state channel
across passes.

The two shared campaign pools are global resource pools, not reservations per
campaign. Their generated capacity is the largest individual `maxParallel`,
which guarantees that no one configured campaign is internally capped while
still allowing concurrent campaigns to contend through tally's ordinary
priority and lease policy. Summing every campaign would silently overcommit the
host by default. An operator who wants aggregate cross-campaign concurrency may
set a larger explicit pool capacity; the per-campaign lower-bound assertions
still apply.

Before admitting the first real run after deployment, verify the selected
implementation adapter on that host:

```console
$ tally adapter smoke codex
```

That command is the activation check introduced by issue #233; campaign
rendering does not depend on its implementation.

An accepted campaign then performs its own command-gate preflight. Every command
gate declares two direct argvs deliberately:

- `preflightArgv` is a base-safe activation probe. It must succeed before the
  first agent dispatch and should exercise the actual compiler, linker, daemon,
  or other estate dependency that can make the later gate unusable. A version
  check alone is insufficient when the gate needs more of the toolchain.
- `argv` is the post-change merge criterion. It may require files that do not
  exist in the frozen spec-only base and therefore is not required to be green
  before an agent has built them.

After the worklist and forge state have been schema-validated and witnessed,
the first pass prepares a separate pristine worktree from the fetched remote
base and runs every command gate's `preflightArgv` there, in declaration order,
as `preflight-gate-<id>`. The declared argv is passed through without rewriting.
Preflight and post-change invocations use the same worktree contract, the same
`runtimeMaxSec`, and `CAMPAIGN_TASK_ID`; during preflight that variable is the
first frontier task ID. If the real merge criterion is itself base-safe, repeat
it as `preflightArgv` explicitly rather than relying on an implicit fallback.

Each preflight records ordinary `exit:0` evidence. A red or timed-out preflight
stops evaluation before the implementation adapter is admitted, so its capture
and witnessed node are the failure receipt rather than an agent cycle spent
discovering the same broken host. Gate IDs must be unique; declarative Nix
configuration rejects duplicates, and direct `tally flow run` arguments are
validated before the worklist node is admitted.

Once **every** command gate's probe has passed, the same lane then runs each
gate's real `argv` once, in declaration order, as `preflight-witness-<id>`. Those
nodes are **not** gates. They declare no `exit:0` evidence, their verdicts are
discarded, and the pass proceeds to agent dispatch whatever they return; a base
that is legitimately red until an agent has built something stays tolerated
exactly as before. Their whole purpose is evidence: the exact merge-criterion
argv, its exit code, and its stderr, on the exact host, at t=0. `preflightArgv`
is only ever a declared-base-safe proxy for that argv, and nothing validates that
the proxy is representative — so an estate-side toolchain defect that the proxy
cannot see is visible in the witness record and the capture file before the first
agent cycle instead of after it. Each witness uses the same worktree, the same
`CAMPAIGN_TASK_ID`, the same `runtimeMaxSec`, and the same `taskRef` as its own
probe. If any probe is red the pass stops there and nothing is witnessed, because
the proxy has already reported the failure.

The two phases are ordered, not interleaved, and the reason is the whole point of
the split. A probe is declared base-safe; a gate's real `argv` is the merge
criterion and is expected to build, write, and mutate. Running one gate's witness
between two probes would hand the second probe a base an unrelated gate had
already changed — so a probe that asserts its own subject is absent on the base,
which is the shape the examples above teach, would go red and the pass would
refuse admission naming the innocent gate. Every probe therefore sees the
pristine base. The witnesses that follow see the base plus whatever earlier
witnesses did to it, which is exactly the order the post-change gate sequence has
always run in.

A campaign's pass node budget therefore reserves two nodes per command gate for
the preflight lane, plus its prep and cleanup. `services.tally.flows.<name>.maxNodes`
is computed from the campaign definition, so no operator action is required.

`forbidPaths` gates are not preflighted because the unmodified base has no task
history to constrain. They begin in their declared position in the post-agent
gate sequence and use their own `runtimeMaxSec` for the packaged driver node.

### Spanning two repositories

A spec-corpus campaign can read its worklist from one repository and land its
work on another. Three roles bind to entries of the campaign's own
`repositories` map:

| Role | What lives there |
|---|---|
| `codeRepository` | Lanes, publish branches, pull requests, merges, merge and checkpoint receipts, and the merged-pull-request scan. |
| `specRepository` | The worklist artifact, read at the revision each pass pins. |
| `issueRepository` | The campaign issue thread and every machine receipt: diagnoses, retries, escalation, and the closing summary. The next-pass continuation is not among them — it is a local events-directory drop and reaches no repository at all. (Also task sub-issues, for the shape described under *staged, not yet reachable* below.) |

Each defaults inward: `issueRepository` falls back to `specRepository`, and
both `specRepository` and `codeRepository` fall back to the repository the
campaign issue was read from. A campaign that sets none of them is a
single-repository campaign and takes exactly the path it took before these
options existed — its rendered arguments do not carry the roles at all.

```nix
services.tally.campaigns.crm = {
  enable = true;
  repositories."mecattaf/crm-spec".checkout = "/srv/spec-repositories/crm";
  repositories."mecattaf/crm".checkout = "/srv/code/crm";
  # The worklist and the campaign thread stay with the spec corpus; the
  # lanes, branches and pull requests land on the product repository.
  specRepository = "mecattaf/crm-spec";
  codeRepository = "mecattaf/crm";
  worklist = "specs/crm/tasks.json";
  # ...
};
```

Two invariants change shape when the roles differ, and neither is optional:

- **The witness splits in two.** The worklist's pinned revision belongs to the
  spec history; every lane base, checkpoint receipt and merged-commit ancestry
  check belongs to the code history. The reconcile result reports both:
  `source.revision` (plus `source.repository`, present only when split) and
  `baseRevision`, the code base tip the pass reasoned from. For a
  single-repository campaign the two are the same commit.
- **Every `owner/name#<n>` is rendered against the repository it resolves in.**
  A short `#<n>` resolves inside the pull request's own repository, so a
  code-repository pull request that writes one is naming a different object.
  The campaign back-reference in each pull request body therefore names the
  issue repository, not the code repository.

An ad-hoc forge-native campaign cannot span repositories. Its worklist, briefs
and receipts are the one issue thread by construction, so a brief carrying the
roles is refused rather than partially honoured.

#### Staged, not yet reachable: task sub-issues on a split campaign

Two behaviours in the driver exist for a shape no campaign can currently be
configured into, and it is worth being exact about which:

- **The full-form closing keyword.** When a task carries its own sub-issue and
  that sub-issue lives on another repository, the publish node emits
  `Closes owner/name#<n>` rather than `Closes #<n>`. The
  [probe on #321](https://github.com/mecattaf/tally.nix/issues/321) verified
  live that GitHub honours the full form across repositories — the sub-issue
  closes on merge, the parent's progress bar advances, and
  `closedByPullRequestsReferences` still returns the merged pull request as the
  oracle — and that the short form links and closes nothing.
- **The cross-repository completion narrowing.** When a closing reference from
  the sub-issue walk names a repository, completion requires it to be the
  campaign's own `codeRepository`. This narrows where proof may come from; it
  never widens what counts as proof.

Neither fires today. A task carries a sub-issue only on the forge-native read
path, and a forge-native campaign refuses the roles above, so the only campaign
shape that can be split is the shape whose tasks never carry sub-issues; the
sub-issue walk is likewise built only for forge-native campaigns. Reconciling
task sub-issues with the worklist-artifact path is design work that has not
been done. Until it is, a split campaign gets the degraded projection: no task
sub-issues, no walk, and machine receipts as comments or refs on the issue
repository.

## The recurring worklist node contract

For a recurring campaign, `worklist` is a relative glob in the configured
remote base tree. It must
resolve to exactly one regular JSON blob and may not contain `..`. The shipped
driver uses the checkout as a Git object store and worktree owner, not as the
authority for uncommitted worklist bytes. It accepts schema version 1:

```json
{
  "schemaVersion": 1,
  "tasks": [
    {
      "id": "customer-model",
      "kind": "implementation",
      "title": "Implement the customer model",
      "goal": "Materialize the frozen customer data contract.",
      "deliveredBehaviors": [
        "valid customer records round-trip without loss"
      ],
      "readFirst": {
        "specSections": [
          "specs/crm/spec.md#r2"
        ],
        "styleReferences": [
          "src/domain/order.rs"
        ]
      },
      "acceptanceCriteria": [
        {
          "id": "customer-round-trip",
          "description": "The focused model test passes.",
          "argv": [
            "cargo",
            "test",
            "customer_round_trip"
          ]
        }
      ],
      "dependencies": [],
      "conflictDomains": [
        "src/domain/customer.rs",
        "src/domain/mod.rs"
      ]
    },
    {
      "id": "phase-one-checkpoint",
      "kind": "checkpoint",
      "title": "Validate the accumulated domain layer",
      "argv": [
        "nix",
        "develop",
        "--command",
        "./test/domain-smoke.sh"
      ],
      "runtimeMaxSec": 900,
      "dependencies": [
        "customer-model"
      ]
    }
  ]
}
```

Every node has an explicit `kind` discriminator. An `implementation` node
requires `id`, `kind`, `title`, `goal`, `deliveredBehaviors`, `readFirst`,
`acceptanceCriteria`, and `dependencies`. `readFirst.specSections` must be a
non-empty list of strings, and that is the whole requirement: nothing resolves
those strings against the filesystem, so a repository with no `specs/` tree
names whatever its lane should read first — a design note, a README section, a
neighbouring source file. The example above cites a spec corpus because that is
the genre it illustrates, not because campaigns need one. `conflictDomains` may be omitted only
while `maxParallel = 1`; every implementation node must provide a non-empty
array when parallelism is enabled. Entries are normalized relative file or
directory paths without `..`. Equal paths and ancestor/descendant paths overlap,
so `src/domain` conflicts with `src/domain/customer.rs`. A reconcile pass
greedily selects ready nodes in worklist order while keeping selected
implementation domains disjoint. Comparisons fold case for portable behavior:
`Docs` also conflicts
with `docs/guide.md`, even when the coordinator's checkout is case-sensitive.
Case-only duplicate declarations are rejected.

The three wire states stay distinct from project input through the canonical
manifest, live task, briefs, and receipts. A non-empty declaration is an
enforced ownership boundary. An explicitly empty declaration allows no changed
path. An omitted declaration is valid only in a serial campaign; the ownership
node accepts its committed change set, certifies every touched path in
`ownedPaths`, and omits `conflictDomains` from its receipt rather than inserting
`[]`.

Immediately after the agent exits, before project gates run, the ownership node
compares the union of paths touched by every task commit with any declared
domains using the same case-folded component-prefix rule. A later deletion
cannot hide a transient unowned path. Adds, edits, deletions, type changes, and
both sides of a rename are included. Publication repeats the check against the
clean exact head before the remote branch or pull request can move, and a
base-changing rebase repeats it before force-push. The flow carries whether
domains are required into each enforcing node, so absence or an empty
declaration cannot turn off the parallel-campaign requirement.

A second, separate node — the tree-delta permission gate — covers what the
ownership gate structurally cannot: content that never reached a commit. `prep`
fingerprints every tracked and untracked path in the lane worktree immediately
before the agent is dispatched, and the gate compares that fingerprint against
the worktree afterwards, so an appearing, disappearing or edited path is caught
whether or not any commit records it.

It runs on both outcomes of the agent node. After a passing agent it runs after
`ownership`; after a *failing* agent — the single most likely context for a
rogue write — it runs in place of `ownership`, since ownership never ran. A
non-empty declaration allows exactly those prefixes, and an explicitly empty
declaration allows nothing, so every delta is a breach. When a serial task omits
the key and ownership ran, the allowlist falls back to exactly the receipt's
`ownedPaths`: the agent's certified committed work is self-authorizing and
nothing else is. When the same omitted task's agent fails, ownership never ran
and there are no certified paths to use. The gate refuses to guess, aborts the
lane as unjudgeable, and preserves the baseline for a later pass with a declared
allowlist. Both implementation schema arms preserve omission into these live
paths; neither producer rewrites it to an empty declaration.

The pre-agent fingerprint is never replaced until a gate has judged the pass it
belongs to. A pass that ends without the gate running — a machinery fault, a
killed runner — leaves its fingerprint in place, and the next pass's `prep`
preserves it rather than re-fingerprinting a worktree that already contains the
previous attempt's writes. Re-fingerprinting there would have made those writes
permanently invisible to every gate that ever ran afterwards.

Ownership results witness the requirement flag, full sorted owned-path set,
base revision, and head. They carry `conflictDomains` only when the task
declared it, preserving both `[]` and non-empty arrays exactly. The tree-delta
result witnesses the task, how many paths it compared, which allowlist
derivation governed, that allowlist, and whether the ownership node had run —
so a reader of a receipt can tell which of the gate's two call sites produced
it. This makes both under-declaration and unused broad declarations visible in
receipts. When enough tasks are ready but overlapping declarations underfill
`maxParallel`, reconciliation emits a diagnostic naming the blocked tasks and
representative overlaps. Shared files such as changelogs and lockfiles
therefore serialize their declaring tasks by design. There is no append-only
exemption: Git still has to reconcile concurrent content edits, so campaigns
that need parallelism should assign those updates to a dependent consolidation
task instead of declaring unsafe sharing.

A `checkpoint` node has exactly `id`, `kind`, `title`, `argv`,
`runtimeMaxSec`, and `dependencies`. Its direct argv is the deeper validation:
an integration scenario, a real-binary smoke, or another accumulated-system
invariant. It has no implementation agent, acceptance criteria, or conflict
domains because it does not implement or publish changes. The direct command still
receives a structured `TALLY_BRIEF` containing its task, workspace, and prior
machine diagnoses, so a retry can observe durable steering. Shell syntax is
never implicit; declare `sh -c` when the checkpoint itself requires a shell.

The checkpoint argv is versioned repository input, not a command selected from
operator configuration. It runs on `campaign-control` with the same execution
options as a Nix-declared command gate. Consequently, anyone authorized to
merge the worklist into the protected base can select code that the campaign
service account executes. This is the same repository-code trust class as a
command gate running a repository test suite, but it removes the operator's
per-command choice. Repository review and base protection—not worklist schema
validation—are the authorization boundary.

IDs are stable node components. Dependencies must name earlier nodes, which
makes the array a validated topological order. Acceptance criteria are runnable,
direct-argv instructions for implementation agents and reviewers; the
campaign's configured `gates` remain the independent merge criterion for
implementation changes. Checkpoints are themselves executable validation nodes,
not an additional operator-facing gate.

Every worklist-specific node also carries the campaign-scoped reference
`<campaign-identity>/<task-id>`. For a forge-native campaign, the identity is
the durable `registrationId`, not the campaign's display name: for example,
`0198f000-0000-7000-8000-000000000042/customer-model`, not
`crm/customer-model`. `tally campaign list` prints each `registrationId` beside
its issue URL and repository, and is the cross-walk from that prefix back to the
campaign. `query.run` keeps the human-readable name in its separate `campaign`
field while `tasks`, `currentNodes`, `failures`, and `anomalies` use the same
registration-scoped refs. File-backed campaigns retain their configured
campaign name as the identity prefix. The reference is additive provenance:
the UUID remains the durable execution identity, while `taskRef` appears in
node receipts, lifecycle and query output, `TALLY_TASK_REF`, unit names, and
capture names. The worklist discovery node has no task ID and therefore no
`taskRef`.

The pass first records its run hash against the sweep node's daemon flow-run
identity. Before deleting any older namespace, it queries the daemon for every
job in that older flow. A paused, queued, or running child protects the entire
run namespace and makes the new pass return `deferred-live-jobs` before
reconciliation. This includes a still-running prep node that has not created or
attached workspace metadata yet. A legacy or malformed lane without a validated
run-to-flow record is left as a safe leak with a witnessed warning; absence of
proof is never interpreted as proof of death. Once an older flow has no live
jobs, the sweep may reclaim its worktrees, local branches, and pass record.

Lane identity is git's own per-worktree configuration, not a file tally keeps
beside it. Preparing a lane enables `extensions.worktreeConfig` on the campaign
checkout and records the campaign, repository, run, task, branch, publish
branch, and base revision under `tally.*` in that worktree's config; the
enumeration a later pass reads is `git worktree list --porcelain` plus that
config. `git worktree add` creates the record and `git worktree remove`
destroys it, so the lane set and the lane identities cannot drift apart the way
the pre-#312 JSON markers under `<workspaceRoot>/.state/` could. The whole
identity is written in one atomic act — a replacement `config.worktree` built
with `git config --file` and renamed into place — because a lane that survives
a kill holding half its identity looks valid to every later pass while being
unable to answer for the half it lost. A lane that does turn up with an
incomplete identity, which is what upgrading an estate across #312 over a live
lane produces, is reported as incomplete and healed from the lane itself; it is
never completed by inventing the missing fields.

Parallel lane roots remain separate, but their linked-worktree records and refs
share the checkout's Git common directory. Fresh lane creation therefore holds
one per-common-directory file lock across `extensions.worktreeConfig` enablement
and `git worktree add`, serializing only the shared metadata mutation.

Resuming a lane means finding a recorded identity that matches; a lane whose
branch outlived its worktree is re-adopted with its work, and a foreign
identity at a lane path is a refusal, never a clobber. **A lane's prepared base
is derived from the lane, not from the base branch's current tip**: it is where
the lane's own head forks from base (`git merge-base`). On a fresh lane that is
the base tip, or the merge base of an existing published head, exactly as
before. On an adopted or healed lane it is an ancestor of the lane head by
construction — which is what ownership, the diff fed to the diagnosing steward,
and rebase all require of it. Recomputing it from a base branch that moved
while the lane was gone would hand every one of them a base the lane's history
does not descend from.

A directory git never registered — a runner killed inside `git worktree add` —
is reclaimed by the sweep from the campaign's own derived lane layout, and the
same sweep reclaims any pre-#312 `.state/<runHash>/<taskId>.json` marker
belonging to this campaign once the run it names is proved dead. The run-scoped
pass record under `.state/passes/` is not lane state and stays where it is. The
same manager serves the agency-nightly driver, so both drivers promise one set
of create/resume/validate semantics.

The reconcile node fetches the configured remote and reads the matching
worklist blob from the exact remote base commit. Uncommitted files and the
configured checkout's local `HEAD` are not worklist authority. It parses,
normalizes, schema-validates, and witnesses the artifact together with its
relative path, SHA-256 digest, and base revision. Forge-native issue worklists
retain their admitted graph digest while witnessing that same live base
revision as non-executable state. The same node queries merged pull requests
carrying tally's exact campaign/task marker, validates the expected checkpoint
refs, and reads authenticated machine comments carrying tally's campaign/task
markers. Merged implementation IDs plus valid checkpoint IDs are completed. A
pull-request proof must also target the configured base, use the stable task
head branch, and have a merge commit contained in the witnessed base. Unknown,
retargeted, or otherwise unusable marked PRs are skipped with warnings in the
witnessed result; multiple valid proofs for one task remain a hard ambiguity.

Two contiguous diagnosis receipts directly block only an incomplete node;
blocking then propagates through its incomplete descendants. Reconciliation
applies `dependencies ⊆ completed` and selects at most `maxParallel` unblocked,
conflict-disjoint nodes. Later nodes use only that witnessed result.

An implementation node receives its one task, assigned workspace, campaign
issue locator, accumulated machine diagnoses, and bounded mission. It is
explicitly told not to read another task from the worklist, to keep every commit
inside its enforced domains, and not to push, open a pull request, or merge. A
checkpoint command receives the corresponding structured retry brief but no
implementation agent. Publication and integration remain separate deterministic
nodes.

A passed checkpoint is recorded as a create-only ref below
`refs/tally/spec-build/v1/<campaign-scope>/checkpoint/`. The expected ref
includes the campaign-and-issue scope, checkpoint ID, full worklist SHA-256, and
exact tested base revision. Changing the declared work graph or advancing the
base requires a new pass. Reconciliation accepts the ref only when it points
directly to that named base commit and every dependency's merge or checkpoint
revision is its ancestor. An older green receipt never certifies a later base,
even when the later commit is unrelated to the checkpoint's declared
dependencies: checkpoints ask questions about the accumulated repository state,
not only their dependency closure.

That namespace is hidden, and deliberately so: it is the same one the
campaign's diagnosis and escalation state already uses, and it is served only
on request. Receipts were published as tags below
`refs/tags/tally/spec-build/v1/` until #307, and **tags are auto-fetched by
every clone** — a private campaign's checkpoint ledger became part of a public
target repository's surface. Already-published tag receipts are still read and
honored, so the move re-executes nothing; nothing new is ever written there.
To clean a target that already carries them, list them with
`git ls-remote --tags <remote> 'refs/tags/tally/spec-build/v1/*'`, confirm the
campaigns they belong to are finished, and delete them under the repository's
ordinary destructive-change procedure
(`git push <remote> --delete <ref>`); the campaign will re-record any still-live
checkpoint into the hidden namespace on its next pass.

Checkpoint refs are immutable and create-only; the driver never force-moves a
receipt. A ruleset should allow the tally forge identity to create refs in
this namespace while denying other identities creation and denying updates or
deletion. If protection denies that identity creation, recording fails closed.
The credential allowed to create these refs is itself a trusted completion
authority—Git cannot prove that its holder ran the witnessed command. The
direct-commit, exact-base, and dependency-ancestry checks reject malformed or
inconsistent receipts; namespace protection keeps unrelated push identities
from minting otherwise consistent ones.

### What binds a forge-native checkpoint receipt

Checkpoints are not a recurring-campaign feature. A forge-native campaign
declares them the same way, as a `checkpoint` task reference in the master
issue's manifest carrying `argv` and `runtimeMaxSec` with its brief in the
sub-issue body, and the driver executes and records them through the same node.
What differs is only where the two halves of the receipt identity come from,
and that is worth being exact about, because a forge-native campaign
re-resolves one of them on every single pass.

A checkpoint receipt is named by `<task>-<source digest>/<base revision>`:

- **The source digest is fixed by `arm`.** For a recurring campaign it is the
  SHA-256 of the worklist blob read at the pinned revision. For a forge-native
  campaign it is the admitted executable-graph digest recorded under
  `$XDG_STATE_HOME/tally/campaigns/armed/`, and Rust poll refuses to dispatch
  when its freshly normalized live issue graph no longer hashes to it. So this half
  cannot drift underneath a pass: it changes only when an operator edits a
  brief, gate, checkout, agent policy, or the DAG and explicitly re-arms — and
  when it does, every checkpoint receipt for that campaign becomes unreadable
  at once, exactly like every merged pull request's revision marker.
- **The base revision is re-resolved every pass.** The forge-native worklist
  node fetches the configured remote and resolves the base branch tip fresh, so
  `source.revision` is a live witness of the code history rather than an
  admitted, digest-covered value. A recurring campaign's `source.revision` is
  the commit its worklist blob was read from and moves for the same reason.

The consequence is one rule with two independent triggers. Reconciliation looks
for a receipt under *this pass's* re-resolved base revision, and accepts it only
when the ref points directly at that commit. A campaign whose base has advanced
since the checkpoint last passed — because of an unrelated push, or a merge from
another campaign — finds no receipt at the new revision and re-executes the
checkpoint there. That is deliberate: a checkpoint asks a question about the
accumulated repository state, so a green answer at an older commit is truthful
history and not an answer about this one. Re-arming an unchanged graph does not
have that effect, because the digest half is byte-identical and every existing
receipt still resolves.

Base movement *during* a checkpoint is the one case that fails rather than
re-running. The record node publishes the receipt for the revision it actually
tested — it is true, and nothing will ever re-test that commit — and then fails
the lane, because that receipt names a revision the next reconciliation will not
read. Reporting it as progress is what would let a base branch moving faster
than the checkpoint runs keep a campaign "advanced" forever. The failure spends
the checkpoint's ordinary steering budget and reaches escalation in a bounded
number of passes. A campaign's own merges all land before its checkpoint lanes
prepare, so only movement from outside the pass trips it.

Two smaller consequences follow for a forge-native campaign specifically. The
digest covers the normalized manifest together with every sub-issue's number,
title, and body, so retitling a checkpoint's sub-issue is as much of an edit as
changing its `argv`: the next pass refuses with "live issue executable graph
does not match the armed digest", and once the operator has inspected it and
re-armed, that checkpoint starts from no receipt. And under the native
sub-issue projection a passed checkpoint writes no progress comment — the
parent's own progress bar is the projection — while a degraded campaign posts
one idempotently marked comment per passed checkpoint.

Old refs are retained as historical audit receipts. Worklist edits and base
movement make them unreachable from the active completion calculation rather
than deleting them. This deliberately preserves stateless recovery and works
with update/delete-protected refs. When a campaign is permanently
decommissioned, its campaign-and-issue namespace can be pruned under the
repository's ordinary destructive-change procedure; there is no automatic
campaign-lifetime inference or in-run receipt garbage collection.

## Reconciliation, parallelism, and the merge criterion

One invocation is one bounded reconcile pass:

```text
sweep old run namespaces only after the daemon proves they have no live jobs
if any old flow still has a paused, queued, or running child: return deferred
implemented = marked merged PRs
checkpointed = valid content-and-exact-base-bound checkpoint refs
completed = implemented + checkpointed
remaining = worklist - completed
diagnoses = authenticated marked diagnosis comments (attempts 1 and 2)
retries = authenticated marked machinery-fault comments (at most 2 per node)
directly_blocked = incomplete nodes with both diagnosis receipts
blocked = directly_blocked plus their incomplete descendants
deferred = incomplete checkpoints with unblocked, unrelated implementation work left
ready = unblocked nodes in remaining whose dependencies are all in completed
frontier = first maxParallel ready nodes with disjoint implementation
  conflictDomains, deferred checkpoints considered last

if remaining is nonempty and frontier is empty:
  -> re-read durable merge, checkpoint, receipt, and steering facts
  -> recompute remaining, blocked, ready, and frontier
  -> if complete or frontier is nonempty: refuse outcome=quiescent; a later pass continues
  -> otherwise post the one marked escalation with accumulated diagnoses -> exit
if implemented is empty, an implementation is in the frontier, and command gates exist:
  prepare an isolated worktree at current remote main
  -> run every command gate.preflightArgv (gating) on the pristine base
  -> then, only if all passed, run every command gate.argv once as a
       non-gating witness
  -> clean up the preflight lane
parallel(implementation frontier):
  prepare isolated worktree -> re-read authorized task-thread steering -> agent
    if the agent passed:
      -> witness ownership -> tree-delta permission gate
      -> each configured gate -> recheck ownership -> push stable task branch
      -> open/reuse PR
    if the agent failed:
      -> tree-delta permission gate against declared conflictDomains
         (an omitted serial task aborts unjudgeable because ownership did not run)
serial(successful publications): compare current base -> rebase if moved
  -> re-run each configured gate only on a changed rebased head -> merge
parallel(checkpoint frontier, after this pass's merges):
  prepare isolated worktree -> run checkpoint argv
    -> retain bounded stdout/stderr for this attempt under capture/archive
    -> record content-and-exact-base-bound completion ref
parallel(machinery faults with retry budget left): marked retry comment
parallel(remaining failed tasks): capture diff -> diagnosis agent
  -> marked steering comment
if any task merged, checkpoint passed, steering or a retry was posted,
or a checkpoint deferred:
  write one bounded continuation event into the events directory
clean every prepared task lane
exit
```

Until the first marked campaign pull request is merged, every fresh pass with a
command gate runs the preflight on a separate pristine-base worktree before
admitting any agent. Each command gate explicitly separates a base-safe
`preflightArgv` from its post-change merge-criterion `argv`. Preflight uses the
first frontier implementation's environment, the same execution host and
deadline as the post-change gate, and a lane that is cleaned before dispatch.
The first merged task is durable forge proof that campaign admission passed;
later passes do not repeat preflight. A checkpoint-only frontier does not
dispatch an implementation agent and therefore does not consume this
implementation admission probe. Because it validates the first frontier
implementation's prepared environment, each preflight node carries that task's
`taskRef` — including the non-gating `preflight-witness-<id>` node that runs the
gate's real `argv` beside its probe.

A checkpoint lane is prepared after this pass's merges, not beside them. A
checkpoint reads the accumulated tree and its receipt is bound to the exact
revision tested, so a checkpoint sharing a frontier with a mergeable
implementation task used to have the pass move the base out from under its own
fresh receipt: the next reconciliation found nothing and re-ran the whole
checkpoint. Prepared after the merges, the tested revision is the one the next
pass reconciles. A checkpoint is still admitted to the frontier alongside
implementation work, and unrelated outstanding work still defers its verdict.

A checkpoint prepares the exact current remote base in its own worktree and
runs its argv as an ordinary settled `campaign-control` node with `exit:0`
evidence, the declared deadline, and the checkpoint's `taskRef`. After every
terminal verdict, a deterministic recording node copies bounded stdout and
stderr tails into that attempt's private `capture/archive` directory before
lane cleanup. These `checkpoint.json` files use the ordinary
`captureArchiveHorizon` (30 days by default), so the existing `tally-retention`
timer prunes them with the other per-attempt captures; see the
[retention policy](../operating/retention.md). On success the driver verifies
that `HEAD` is still the prepared base, no tracked file changed, and the
prepared base still belongs to the current remote-base ancestry. It then
publishes an immutable receipt for the exact revision that was tested, plus a
progress comment where the degraded projection is in force. If the
remote base advanced during validation, the receipt is still published — it
remains truthful historical evidence and nothing will re-test that revision —
but the lane then fails, because the receipt names a revision the next
reconciliation will not read. That failure is the bound on the re-validation
loop: a base branch advancing faster than a checkpoint runs would otherwise
record, be ignored, and be re-run for ever while every pass reported progress.
Failing spends the checkpoint's ordinary retry and steering budget and reaches
escalation instead. Because a campaign's own merges all land before its
checkpoint lanes prepare, only base movement from outside the pass trips it. A
diverged or force-replaced base fails closed. The pass-wide continuation is
written after every lane settles, including after a checkpoint failure has
published machine steering; checkpoint recording adds no second retry loop.
Ignored or untracked build outputs are allowed and removed with the worktree.
There is no implementation agent, configured per-task gate sequence,
publication branch, pull request, rebase, or merge for this node kind.

The agent must leave a clean worktree with at least one commit descended from
the prepared base. Ownership validation then fails before the more expensive
project gates if any commit touched an undeclared path. Publication independently
refuses dirty, empty, non-descendant, or newly unowned work.

Ownership validation and every `forbidPaths` gate read the same range: from the
base branch commit this lane sits on to its head. That start is the merge base
of the lane head with the current base branch. It equals the prepared base for
a lane that never moved, and the rebased-onto revision for a lane that took the
documented remediation for a red constraint, so a rebased lane owns only its
own commits instead of every mainline path landed since it was prepared. Unlike
the current tip, it does not move when the base advances again behind the lane,
so a gate receipt and the publication that re-checks it count the same paths.
The receipt keeps naming the base the lane was prepared and gated on, and the
start can only move forward from that base, so nothing this resolution does
widens what a lane may touch.

The current base branch tip is read by fetching it in the lane and resolving
`FETCH_HEAD`, never by reading `refs/remotes/<remote>/<baseBranch>`. A lane
worktree is a linked worktree of the campaign checkout, so that ref lives in
the shared common Git directory and anything running in the lane can write it —
including the agent whose ownership is about to be validated. Pointing it at
the lane head would otherwise collapse the range to nothing and satisfy every
declared domain vacuously, in the one check that exists because the agent is
not trusted to stay inside its lane.

The union walks that range with `git log -m`, which splits a merge commit and
attributes both of its sides to the lane. A lane that merged the base branch
instead of rebasing onto it would therefore claim every path its siblings
landed. Such a lane is rejected by its real cause — "rebase instead of merging
the base into your lane" — rather than reported as an ownership violation on
paths no task commit touched; `--first-parent` would hide the false positive
and reopen the transient-path hole with it. Only merges the lane itself
authored are read: every campaign merge is `--no-ff`, so a base branch that has
integrated anything is full of merge commits, and a lane that rebases onto it
inherits them. Reading from the stale prepared base would reject that lane for
doing exactly what the steering text tells it to do.

Publication answers to the campaign's configured gates, not to the receipts it
is handed: the witnessed `forbidPaths` sets are cross-checked by gate id
against the configured ones and any drift fails by name. Re-running a stored
receipt against itself would otherwise let a campaign whose patterns were
widened between the gate run and publication publish against the superseded
set. Integration applies the same rule to the ownership receipt's
`domainsRequired` bit, which merge compares against the campaign's own
parallelism rather than normalizing and trusting.
Each task has a stable remote branch across passes and a run-local worktree lane,
so a dead runner cannot make a later pass share a writable directory with an
old child. Pass-exit cleanup reclaims every prepared lane, including failures;
the next pass's daemon-backed sweep defers while an old child is live and covers
a process that died before cleanup only after every admitted child settles.

Publications may finish in parallel, but integration follows deterministic
frontier order. Before each merge the driver fetches current base. If the
already-gated head contains it, integration is a no-op. If base moved, the
driver rebases with an exact force-with-lease, tally re-runs every configured
gate on that new head, and merge refuses if either base or task branch moved
again. Thus concurrent implementation does not weaken “witnessed gates are the
merge criterion." A dependent task cannot enter any frontier until its
prerequisite PR is observed merged by a later pass.

### Squash merges and the steward's narration

`mergeMethod` decides how the merge node integrates a task, and the campaign
default is `squash`. The footprint a campaign should leave on the forge is one
conventional commit per task, not a merge commit carrying a template message.
Under `merge` the driver runs `gh pr merge --merge --match-head-commit <head>`
and proves completion by requiring the task head to be an ancestor of current
base. A squash never makes the task head an ancestor of anything, so under
`squash` the driver runs `gh pr merge --squash --match-head-commit <head>
--subject <subject> --body <body>` and proves completion from the pull request
instead: state `MERGED`, a full merge-commit object ID, and that merge commit
contained in current remote base. That is the same oid the reconcile read path
already validates against the witnessed base, so the read path needed no
change.

On a `forge = "local"` campaign the merge node runs `git merge --squash`
followed by one commit on base carrying the validated message. A squash leaves
no ancestry a later pass could read, so the node also publishes a receipt ref
under the campaign's hidden state namespace naming the commit it produced.
Reconciliation reads both proofs on every pass — branch-head ancestry and the
receipt — so a campaign whose `mergeMethod` changed between passes still sees
its earlier merges. A receipt proves nothing on its own: the reader still
requires the commit it names to be an ancestor of the witnessed base.

The message that squash commit carries comes from the steward. `steward` names
an adapter in the open adapter map, bound as a catalog role; `stewardArgv` is
appended to that adapter's own argv, and the result is the direct argv the
publish node runs. Three fields of that adapter entry reach the seam — `argv`,
`env`, and `scrape.finalMessage` — and between them they decide which model
answers, at which endpoint, with which credentials, and how its proposal is
read. Swapping narrators is an adapter change; nothing about the model belongs
in the campaign's own options.

What the seam does **not** read is the adapter's per-job `launch` policies,
`hardening` preset, and `extraWritablePaths`. The narrator is a direct-argv
subprocess of the publish node, not a tally job — that is what keeps the seam
free of flow nodes — so nothing applies them. A steward adapter that declares
any of them is refused when the module is evaluated, rather than run without
them: an estate that believes its narrator is sandboxed should not learn
otherwise from a journal. For the same reason the narrator's environment is the
publish node's, plus the adapter's `env`, minus `TALLY_BRIEF`: it is handed its
request on stdin and has no business reading the driver's own brief.

The publish node writes a JSON narration request on the narrator's stdin and
reads its proposal back from the line matching the adapter's declared
`scrape.finalMessage` regex, which defaults to the `TALLY_FINAL_MESSAGE=`
contract the shipped `spec-build-driver` adapter scrapes. That regex must be a
non-empty stdout `regex` capture with exactly one group, or the module refuses
the binding. The proposal is text only: an object with `type`, `scope`,
`subject`, and `body`.

A deterministic, commitlint-shaped validator then decides whether that text is
used. It requires a conventional type from a fixed set, an optional short
lowercase scope, a `type(scope): subject` header of at most 72 characters with
no trailing period and no leading capital, a body under 4000 characters wrapped
at 100 columns, no control characters, and no managed campaign marker anywhere.

It also refuses two things that are not style at all. A pull-request body is
executable on GitHub: a closing keyword in a merged body — or in a commit
message that lands on the default branch, which is exactly what the squash
message does — closes the issue it names, and an `@mention` notifies a person
or a whole team. The node appends its own `Closes #<sub-issue>` because that
authority belongs to the node; a narrator proposing `Closes #310` is proposing
to close the design issue that authored the campaign. Both are refused with a
named reason. A bare `#<n>` cross-reference stays allowed: it backlinks and
notifies nobody.
A refused proposal is re-requested once with the refusal reason attached. A
second refusal — or a narrator that exits nonzero, misses its deadline, or
prints no final message — spends the slot, and publication falls back to the
brief-derived template. The campaign proceeds either way; narration never
blocks a merge. The model proposes, the validator enforces, and the node runs
git. The narrator is never given git.

The narration governs the pull-request title and the prose above the managed
marker at publication, and the squash commit's subject and body at merge. A
pass that reuses an already-open pull request leaves that pull request's text
alone — it was authored by the pass that opened it and carries the campaign's
identity marker — so on a re-published task the freshly narrated message
reaches the squash commit and not the pull request. It
adds no flow node: the publish node runs the narrator itself, so a campaign's
node budget is the same with a steward as without one. With no steward
configured the narration is the template, and the published text is byte for
byte what it was before the seam existed.

If publication finds that the task head has the same tree as its witnessed
base, the pull request is an intentional campaign marker rather than a code
change. Its title is prefixed with `[marker] ` so reviewers and automation can
distinguish it from a substantive task PR without opening the diff.

An implementation brief may ask the worker to report a judgement, trade-off,
or other finding. The honored reporting surface is the worker's final message:
when the resolved agent adapter declares `scrape.finalMessage`, a completed
implementation lane carries that capture into publication and posts it as a
marked comment on the task's sub-issue thread. A campaign without native
sub-issues uses its campaign issue as the historical fallback. The driver
applies the public redactor and bounds the complete UTF-8 comment to 8 KiB;
an absent or blank capture posts nothing. Module-declared campaigns refuse an
agent adapter without that capture, while `tally campaign arm` also warns that
“worker findings will not be retained” for direct or older resolved configs.

The implementation worker still does not author pull-request text. A brief
request to “put this in the PR body” is not honored: from the worker's point of
view the title and body are template-only and its finding lands in the comment
above. With no steward, the driver template is the whole pull-request body; a
configured steward may alter only the separately proposed, validated narration
described in this section. It does not turn the implementation worker's final
message into pull-request prose.

### The provenance trailer and the post-merge git-ai binding

The squash commit is where a campaign's authorship becomes repository-native
(`specs/eta/evidence/day-docs/AUGUST-01-DESIGN.md` §7). Two separate things land there, and only one of
them is proof.

The **`Assisted-by:` trailer** is the pointer. The merge node appends
`Assisted-by: <adapter>:<model> (tally:<taskUuid> witness:<seq>)` to the squash
message, byte-identical to what the gh producer already publishes on a
completion comment. Every component comes from the settled implementation node:
the campaign's agent adapter, the canonical model the daemon recorded for that
execution, and the task UUID and witness sequence of the attempt that produced
the head being merged. When the estate never named a model — `agentModel` is
null and the adapter declares no model override — there is no canonical model
to name and the node writes **no trailer** rather than a plausible one. A
narrator that proposes an `Assisted-by:` line is refused for the same reason it
is refused a closing keyword: that authority belongs to the node. The refusal
matches the way git reads the line, not the way the node spells it — git
matches trailer keys case-insensitively, so `assisted-by:` is the same trailer
and is refused too. That matters most under the shipped default: with no model
named the node appends nothing, so a forged line would be the message's entire
trailer block. A `merge` integration gets no trailer, because git writes its
own message there and the working commits it collects keep their own notes.

The **note on `refs/notes/ai`** is the proof, and reaching it takes deliberate
work. `gitAiBinding` selects the posture: `off` (the shipped state), `advisory`,
or `required`. `doc/src/flows/git-ai-squash-fidelity.md` records the
measurement the binding is built on — attribution is re-minted per line, by
git-ai's background service, at `git commit` time, and only in the repository
that made the commit. A forge-side squash therefore arrives unbound, and
nothing about fetching or reading it recovers the attribution.

So under `advisory` and `required` the merge node, after the merge is proven,
mints the same integration a second time: a detached worktree of the campaign
checkout — the one place that still holds the task branch's checkpoints and
shares its `refs/notes/ai` — squashes the same head onto the same base,
commits under its own identity, and waits on `git-ai await` for
`gitAiAwaitSec`. It then proves the reconstruction is the integrated commit's
content, not merely something like it: the merged commit's first parent must be
the gated base, and the reconstructed tree must equal the merged tree. Only
then is the note copied onto the integrated commit's object ID. The
reconstruction's own entry is then removed — a notes entry is keyed by commit
id as a path in the notes tree, so it outlives the commit it annotates, and
leaving it would accumulate one dead note per merged task.

Publication is scoped to that one entry. The campaign checkout's
`refs/notes/ai` accumulates a note for every commit the shared checkout has
ever made, including abandoned attempts and diagnosis commits, and none of that
was ever chosen for a public forge — so the node assembles a scratch ref from
the remote's own tip plus the integrated commit's note and pushes *that*. It is
never forced, and two notes for one commit are never merged: a remote already
carrying a **different** record for this revision is reported as a typed
`conflict` and nothing is written or pushed. `cat_sort_uniq` — git's own
line-oriented note-merge strategy — cannot be used here, because a git-ai
`authorship/3.0.0` note is a two-section record whose line order is semantic;
folding two of them yields a structurally invalid note under a schema version
it no longer satisfies, and because `git notes merge` writes into the *local*
ref it would rewrite the daemon's witnessed code-result bindings in the
campaign checkout at the same time.

Scoping what tally publishes is not the same as controlling what the remote
carries: git-ai publishes `refs/notes/ai` itself on an ordinary `git push`,
measured in `doc/src/flows/git-ai-squash-fidelity.md`. The merge node's
contribution is exactly one entry; the rest is the estate's own tool.

The barrier runs inside the merge node, so `gitAiAwaitSec` and
`driverRuntimeMaxSec` are not independent: evaluation refuses a deadline below
twice the budget whenever the binding is armed. A campaign whose node deadline
does not comfortably exceed the barrier is killed mid-wait on every task and
reports a node timeout instead of a binding receipt.

Every outcome is journaled with the merge node as an `authorship` receipt
naming the binding posture, a status (`bound`, `unavailable`, `missing-note`,
`mismatch`, `conflict`, or `error`), the bound revision, the campaign remote's
`refs/notes/ai` target after publication, the SHA-256 of the exact note bytes
**read back from the remote**, whether the push succeeded, and a typed reason.
Under `advisory` that receipt is the whole consequence, and that is enforced
rather than promised: the merge has already landed irreversibly by the time the
binding runs, so an advisory subsystem that raised would report a merged task as
failed. Every outcome — including an unexpected one — becomes a typed receipt.
Under `required` any status other than a published `bound` fails the merge node.

That receipt is also the verification handle. The witness ledger records the
daemon's settlement-barrier binding on the *coder's* result revision; the merge
node binds a different object, the commit the forge minted, and writes nothing
to the ledger. The two mechanisms attest different commits and never meet, so
`verify-authorship` takes a revision mode:

```console
$ tally witness verify-authorship \
    --repository /path/to/checkout \
    --revision <authorship.revision> \
    --note-sha256 <authorship.noteSha256>
```

It re-derives the digest from the repository and compares it against the claim
the receipt recorded. The digest is required, so a pass is always a comparison
and never a vacuous "a note exists".

Arm `advisory` first, and not as caution theatre: an unprovisioned host and a
squash that lost its attribution produce byte-identical evidence — no note — so
only real squash merges can show that the binding works. `required` also
couples every campaign merge to one externally provisioned binary version,
which tally.nix does not ship. The binding adds no flow node; it is a step
inside the merge action that already existed.

The daemon-side `services.tally.gitAi` settlement barrier is a separate
mechanism with a separate switch. It binds the code result's own revision at
completion; this binds the commit the campaign integrated. Neither implies the
other, and `services.tally.gitAi.globalAwaitOk` stays `false` under parallel
lanes because that barrier is process-wide.

If the published head conflicts with current base, the driver aborts the rebase
and deletes only that exact leased remote head. Pass-exit cleanup removes the
failed lane. The next reconcile attempt therefore prepares the task from
current base and lets the agent redo it; it cannot resurrect the same
unrebasable head indefinitely. A closed GitHub PR on the stable branch is
reopened when the replacement head is published.

A preflight failure stops the pass before any agent is admitted. Agent,
ownership, task gate, checkpoint, publication, rebase, and merge failures are
settled into the pass report. A failed implementation remains unmerged and a failed checkpoint
publishes no completion ref. Either failure is diagnosed after its lane settles;
successful conflict-disjoint siblings still publish, record checkpoints, and
merge. The first marked diagnosis leaves the node eligible for one fresh retry.
The second marks that node directly blocked. Blocking propagates only through
its dependency descendants, so unrelated ready subtrees continue to advance.

## Failure, steering, and re-entry

Recurring campaigns use their authenticated issue comment channel for steering.
An ad-hoc issue campaign instead snapshots only comments authored by its local
`allowedActors`; the agent receives that bounded value in its immutable brief
and does not reread the public channel. A deterministic node performs the one
late task-thread re-check described above before the agent is admitted; tally
never changes a running node's immutable brief. After a task node fails, a
separate diagnosis agent receives four explicit inputs: the failed node's
bounded capture stderr, every gate
output collected for the task, the exact task brief, and a bounded diff against
its witnessed base. The diagnosis agent is told not to modify the repository or
repeat secret-looking input. Only its concise output passes through conservative
public redaction and becomes an authenticated, marked campaign comment; raw
capture, gate output, brief, and diff remain private job inputs.

The diagnosis prompt states the public outcome-first grammar and includes a
conforming example. For a failing gate it also states the literal-substring
contract explicitly: the response must contain the failing check id and, when
the gate names one, the offending path without paraphrasing. The prompt renders
the exact required strings for that failure before the diagnosis agent runs;
the driver then supplies those same strings to the unchanged validator.

If the diagnosis agent still returns invalid grammar, the rejected prose is not
admitted as valid. The driver immediately posts a marked `grammar-rejected`
machine-steering receipt: a deterministic, grammar-valid wrapper names the
rejection reason and required literals, then carries a bounded excerpt after
public redaction. The receipt is therefore visible in the next worker's
`machineDiagnoses` just like compliant steering instead of being withheld as a
machinery retry. A diagnosis rejection cannot erase useful failure evidence or
weaken the public grammar.

The pass then writes its continuation event even when nothing merged, and
even when the diagnosis lane itself faulted: a transient adapter failure must
never leave a campaign stopped with neither steering nor a mention to resume
from. The next event has a fresh flow-run identity, re-reads forge state, and
includes the first machine diagnosis in the implementation or checkpoint brief.
A second failure produces attempt 2 and blocks that node. Because attempts live
in forge comments, not runner memory or a campaign-local checkpoint, a redeploy,
crash, timer, or fresh mention derives the same scheduler state.

Only evidence that the task's work is wrong spends an attempt: a non-zero agent,
a rejected ownership boundary, a red gate or re-gate, and a red checkpoint
command. Campaign machinery — preparing a lane, an unexpected lane exception,
rebasing, publishing, and merging — says nothing about the work, so a fault
there posts a marked retry comment instead, and the pass writes its
continuation event so the retry is actually taken. That retry
budget is bounded at two per task and is read back from the forge like every
other campaign fact, so a permanently broken lane still spends its two steering
attempts and reaches escalation rather than retrying forever.

The Codex adapter has one narrow additional classifier. A 2026-08-10 bare
`codex exec --json` probe on Codex 0.147 survived a destructive tool-router
rejection, emitted a completed turn, and exited zero; the 0.145 deaths reported
in #452 are therefore treated as version-specific adapter session failures,
not a tally stdin-wrapper failure. If an agent-stage failure's captured stderr
carries the `codex_core::tools::router` signature, the campaign prices it as
machinery first. Ordinary Codex nonzero exits remain task evidence and spend
steering in the usual way.

### Daemon congestion and the advisory projection wait

After a flow node's exit evidence has passed, the flow host keeps polling the
daemon for the node's advisory `finalMessage` projection — the structured result
a driver such as `spec-build-driver` prints on its last line. The daemon's
dispatch loop can briefly stall under load, and while it does the projection
does not move. The host therefore does not give up at the first empty poll: it
retries with a bounded exponential backoff inside a configurable wait, and a
projection that lands late still completes the node, so the pass survives the
stall instead of dying on work that already succeeded.

If the projection is still absent when the wait is exhausted, the node is
classified `retryable-projection` — "projection unavailable within N ms; daemon
congested?" — and never `result-schema-mismatch`: congestion is not a contract
violation, and an operator grepping a dead campaign can tell the two apart.
(`retryable-projection` is raised only when the exit evidence passed; a node
whose exit evidence failed keeps the older `result-projection-timeout`.)

A terminal result that already carries a structured tally-side error does not
enter this wait. In particular, executor request validation is synchronous and
returns immediately as `executor-validation-failed`; its original validation
message remains the flow error and the witnessed/queryable terminal cause.
There may be no capture for such a node because its payload was never launched.

The wait defaults to 10 s and is widened per campaign at arm time:

```
tally campaign arm <issue-url> --projection-wait-ms 240000
```

An explicit value is recorded in the registration's versioned host-tuning
sidecar and put on the argv of every `tally flow run` the campaign dispatches —
the arming pass and every later pass `tally campaign poll` dispatches — as
`--result-projection-wait-ms`. Arming is the seam because a campaign pass runs
as a daemon-launched transient unit whose environment is built from an explicit
`--setenv` list: nothing an operator exports in their own shell reaches it, so
an environment-only knob would not have been reachable from where campaigns are
actually started. Re-arm with a different value to change it; re-arming without
the flag clears it back to the 10 s default.

Rolling the binary back one generation leaves the schema-2 authority readable.
That older binary does not scan the sidecar and therefore uses its historical
10 s wait even when an override is present. It may continue polling and updating
observation fields without changing the approved digest or pinned paths. After
rolling forward again, the current reader joins the unchanged authority to the
retained sidecar and restores the override.

It stays out of the campaign manifest deliberately. The manifest is hashed into
the executable graph digest, so putting a host tuning knob there would make
"wait longer for this host's stalls" a change to what was approved.

A `tally flow run` you launch yourself takes the same value directly, either as
`--result-projection-wait-ms MILLISECONDS` or from the
`TALLY_RESULT_PROJECTION_TIMEOUT_MS` environment variable; the flag wins, and a
zero or unparsable value is refused rather than silently falling back to the
default.

Set it comfortably above the longest stall you have observed; it only bounds a
wait, never changes what a node asserts.

A checkpoint reads the accumulated tree, so a red verdict while unrelated
implementation work is still outstanding says nothing about the checkpoint. Such
a run is a deferral: it spends no attempt, and the reconciler considers a
deferrable checkpoint last so it never displaces real work from a bounded
frontier. Tasks that are already blocked, and tasks on either side of the
checkpoint's own dependency chain, cannot change its verdict and so never defer
it — the campaign still reaches quiescence.

Between passes an operator may rename, drop, or re-scope worklist tasks. Machine
receipts naming a task the worklist no longer has, and receipts left without the
attempt that should precede them, are reported as reconciler warnings and
ignored. A worklist edit degrades the campaign's memory of past attempts; it
never bricks the campaign.

Escalation is a state transition, not the first failure: it occurs only when the
worklist is incomplete and the recomputed unblocked frontier is empty. The
terminal action re-reads all durable facts immediately before publication and
refuses to post `outcome=quiescent` if that refresh completes the campaign or
reopens any frontier. Only a still-empty refreshed frontier reaches the one
marked escalation containing compact summaries of all machine
diagnoses, plus every checkpoint capture path retained in those diagnoses and
machinery retries, and never posts it again for that campaign issue. Start investigation
with `tally query run <runner-task-uuid>`, adding `--status blocked` on a large
worklist: its task table identifies the blocked campaign task and failed stage,
and its failure section carries the retained capture path — reading
`<not retained>` when no capture was kept, so an absent pointer is never
confused with an unresolved one — and the bounded stderr tail. Use
`tally query log --flow-run <runner-task-uuid>` only when transition or
provenance history is needed. A public campaign receipt is absent by default; it
includes failure metadata only with `postFailureEvidence` and a conservatively
redacted tail only with the additional `postFailureStderr` opt-in. Task-specific
records retain `taskRef`, so the worklist ID is visible without a UUID lookup.

An operator may add a steering decision using an authorized actor; the ad-hoc
poller observes that comment automatically. If executable campaign content
changed, review it and explicitly re-arm instead.

An operator can then repair and merge a marked task PR or otherwise resolve the
forge state before posting a fresh mention. Preflight remains outside this
task-attempt protocol because it proves campaign admission before any task agent
runs; repair its host or base defect and re-enter with the configured mention.

A pass that reaches its own preflight verdict — green or red — always cleans the
preflight lane before it returns or throws, so an ordinary red preflight leaves
no residue. The one case that does is a runner killed while preflight is still
running: the `_campaign-preflight` worktree under
`<workspaceRoot>/<repository>/<runHash>/` and its
`tally-work/<campaign>-<runHash>/_campaign-preflight` branch outlive the process
that made them. Nothing needs to be removed by hand. The next pass's sweep node
claims the same namespace: it recognises `_campaign-preflight` as a campaign
lane name, proves through the daemon that the dead pass has no live child, and
then removes both the worktree and the branch, reporting each as a `cleaned`
entry. If a preflight job from the killed pass is still running, the sweep
refuses to touch anything and returns `deferred-live-jobs` instead of racing it;
post the mention again once it settles. The recovery path is therefore the same
one every other campaign failure has — post the configured mention — and never a
manual `git worktree remove`.

```console
$ gh issue comment ISSUE --repo OWNER/REPO --body '<configured mention>'
```

That recurring event—or a new authorized ad-hoc observation—creates a fresh
flow-run identity. The pass does not reuse or repair an old runner prefix: it
observes merged PRs and checkpoint refs again, re-reads diagnosis and escalation
comments plus authorized steering, recomputes the whole frontier, and gives an
eligible failed node a new isolated lane with current steering.
Changing campaign arguments or deploying a new content-addressed script between
passes is ordinary generation change, not replay divergence. A module-declared
continuation carries forward the arguments of the pass that wrote it, so a
redeploy reaches a running campaign at its next mention rather than mid-chain;
a forge-native continuation re-reads the registry and therefore picks the new
generation up immediately. Duplicate mentions
are safe because the campaign mutex serializes passes and each pass re-derives
the same forge facts before dispatch.

Each pass contains at most one bounded frontier, so the fixed 24-hour evaluator
budget no longer measures the whole campaign. Both campaign classes continue the
same way and neither posts a public self-nudge: a pass that merges a task,
passes a checkpoint, diagnoses a failure, posts a machinery retry, or defers a
checkpoint writes one bounded JSON enqueue payload into the daemon's
events directory, and the 5s drain admits the next pass. A module-declared pass
writes its own flow-run argv under a derived run identity; a forge-native pass
writes the same registry scan the timer runs, so the next pass inherits the
`campaign:<repo>:<number>:<revision>` dedup key. The human at-mention stays
public and remote; only the machine's note-to-self moved local.

Double-triggering is safe by construction, which is why the timer can stay armed
underneath. The campaign pool is a capacity-1 mutex, so passes serialize; and
the continuation payload carries a deterministic `dedupKey` under full
submission, so a duplicate event — or a race between the event and
[`tally-campaign-poll.timer`](#arm-an-ad-hoc-issue-campaign) — resolves to an
attach or reuse against the pass already admitted rather than a second pass.
The timer therefore remains the recovery path for a lost event. If the pass
process dies before producing that durable
outcome, wait for any admitted children to settle and post a fresh mention.
Stable remote task branches preserve published work; merged PRs preserve
implementation completion, checkpoint refs preserve successful automated
barriers, and marked issue comments preserve failure attempts and the one
escalation. A calendar producer is not an implicit campaign timer: its payload
is static, while issue intake supplies the dynamic repository, issue number,
URL, and forge event identity.

Generic flows that truly require one run identity still use [submission
identity and replay](submission-and-replay.md). Spec-build deliberately refuses
a flow-run ID once its sweep node would be `reused`: frontier branches execute
concurrently and do not promise the same ordinal interleaving. Reattaching to
the still-live first sweep is safe because no frontier has yet been derived.
Recovery after a completed sweep must use a fresh mention or continuation and
therefore a fresh forge event ID.

## Starting recurring automation

This is the heaviest of the three weights, and everything below is what
*recurring estate configuration* costs. None of it is a precondition for
running a campaign: a local worklist campaign needs none of these steps, and a
repository with no spec corpus at all is a supported campaign target.

The complete operational sequence is:

1. Freeze and commit the spec corpus, including its schema-versioned task
   artifact and style-transfer references.
2. Provision its writable checkout and adapter authentication on the tally
   host.
3. Open one issue in that repository with the configured label.
4. Add one `services.tally.campaigns.<name>` attrset and deploy Home Manager.
5. Run `tally adapter smoke <agent>` on the deployed host.
6. Post the exact configured mention on the issue.

That is the recurring activation path: no per-repository flow script, dispatch
wrapper, producer block, or extra serialization service. For a one-night or
otherwise ad-hoc buildout, stop before steps 3–6: project the worklist and run
`tally campaign arm` instead. For ordinary work on a repository that has no
spec plane, drop to the local worklist weight — [scaffold, arm, push,
release](small-worklist.md) — which needs neither the corpus of step 1 nor the
forge and module surface of steps 3–4. Promoting a repeated campaign into this
declarative surface is an explicit change of weight class.
