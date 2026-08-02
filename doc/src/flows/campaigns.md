# Campaigns

Tally has two campaign weights:

- **Ad-hoc campaigns are forge-native.** One GitHub master issue contains the
  campaign configuration and ordered DAG. Its native sub-issues contain the
  exact per-task briefs. `tally campaign arm ISSUE-URL` registers that durable
  object without a Nix edit, generation build, fleet deploy, or orchestration
  commit in the product repository.
- **Recurring campaigns are declarative.** `services.tally.campaigns` remains
  the right surface when the same label, mention, repository policy, and
  worklist discovery should be estate configuration.

Both modes use the same shipped, bounded, stateless `spec-build` reconciler.
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
  content-and-exact-base-bound checkpoint tags are the completion facts read by
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

Campaigns are a Home Manager feature on both deployment paths. The Home Manager
module installs the generic campaign pools, packaged flow and driver, and
`tally-campaign-poll.timer` once. The NixOS module deploys only the daemon and
asserts that `services.tally.campaigns` is empty, so it renders no campaign
pools, no spec-build flow, no driver adapter, and no poll timer; a campaign
armed against a NixOS-only host would have nothing to dispatch into. Run
campaigns from the Home Manager module.

The timer only scans locally armed
issue locators; an empty registry performs no work. A pass that advanced admits
its own successor through the events directory, so the timer is the recovery
path for a lost continuation event and the way an outside change to the issue
graph is noticed, not the ordinary way a campaign reaches its next pass. It
scans every `services.tally.campaignPoll.interval` (60s by default). A scan holds the registry lock exclusively across its forge
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
        "id": "tests",
        "preflightArgv": ["nix", "develop", "--command", "cargo", "--version"],
        "argv": ["nix", "develop", "--command", "cargo", "test", "--workspace"],
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
and conflict domains. A `checkpoint` reference carries `id`, `kind`, issue
number, dependencies, direct `argv`, and a positive `runtimeMaxSec`; unsupported
kinds fail closed rather than becoming implementation work. The full human
brief or checkpoint description is always the sub-issue body. The manifest task
set must exactly equal GitHub's native
sub-issue set, task IDs form a topological order, and forge-native campaigns are
bounded to 100 tasks by the native sub-issue API. Drift fails closed before
admission. Directly authored sub-issues need no tally marker: their native
parent relation plus the manifest's issue number bind each brief to its task.
The task-body marker is projection ownership metadata added only by `project`;
an explicit `issue` field lets `project` adopt an unmarked task issue.

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
`Closes #<sub-issue>`. Completion identity includes the admitted graph digest:
editing a brief, gate, checkout, agent policy, or DAG after a merge cannot reuse
the old PR as proof.

### The sub-issue walk, and what a closed sub-issue does not prove

`arm` probes once, before it registers anything, whether this forge can serve
the walk the native read path needs: parent → `subIssues` →
`closedByPullRequestsReferences` → `pullRequest.merged`, in one bounded
GraphQL query per pass, paginated within the 100-task cap. A forge that refuses
the query is a capability answer, not a campaign failure — the campaign arms in
degraded mode, `arm --no-enqueue` reports which mode it recorded, and every
projection above falls back to checkboxes. Re-arm to re-probe.

The walk narrows **where** completion candidates come from; it never widens
**what** counts as proof. A pull request reached through a task's sub-issue
still completes that task only if its body carries the exact revision-bound
marker for the admitted graph and it passes the same base branch, stable head
branch, merge-commit and ancestry validation as before. A pull request from a
pre-edit graph is named in the pass warnings and counts for nothing.

A sub-issue is human-clickable, so its closure carries no authority at all.
`pullRequest.merged` remains the only oracle. When the walk finds a sub-issue
closed while its task holds no revision-valid merged pull request (or, for a
checkpoint, no completion ref), the task stays incomplete and in the frontier,
and the pass records a typed `closed-without-merged-proof` anomaly.
`tally query run RUN-UUID` prints those anomalies above the task board and puts
the run in `needs-attention`; they are not filed with the reconciler's
warnings, because a reader who misses one debugs the wrong surface.

### Per-task steering threads

Where the walk is available, each task's sub-issue is that task's steering
thread. A machine diagnosis or machinery-retry receipt for task `T` is posted on
`T`'s sub-issue and read back from there, so `T`'s retry brief carries `T`'s
history and no other task's. A comment by an allowed actor on `T`'s sub-issue
reaches `T`'s agent as `steering.authorizedComments` and advances the
observation revision exactly like a master comment does. The master issue stays
the campaign-wide channel: campaign-level human steering reaches every task, and
escalation and the closing summary are always posted there. A receipt left on
the master by a pre-#307 pass is reported and ignored rather than double
counted; receipt parsing follows the posting surface.

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

The poller may refetch projection state and allowed-actor comments, but it
refuses any changed executable graph until the operator inspects it and runs
`arm` again. The reconciler independently refetches and recomputes that digest
before executing. Checkbox/state/timestamp changes are observation signals, not
executable changes. Agents receive a bounded authorized steering snapshot and
are told not to refetch the public comment channel. Re-arming the same unchanged
graph forces a fresh retry without invalidating matching completion facts;
re-arming changed content creates new task revisions. `--no-enqueue` registers
after validation without starting a pass, and `--wait` returns its terminal
verdict.

The default ad-hoc `workspaceRoot` is
`$XDG_CACHE_HOME/tally/campaigns/workspaces` (or
`~/.cache/tally/campaigns/workspaces`), outside tally's state/data byte budgets.
Override it when those workload-owned lanes need a dedicated monitored volume.
`forge: "local"` is rejected unless `--allow-test-local-forge` is supplied; it
is a mechanism-test mode and has no autonomous continuation contract.

Merge/checkpoint projection changes and allowed steering comments advance the
observation revision, so the poller submits a fresh pass behind the capacity-one
campaign mutex. When every task has durable proof, reconcile repairs all boxes,
closes task issues, posts one digest-bound summary, and closes the master. The
next poll prunes that registration. Operators can remove an open registration
without changing forge state with `tally campaign disarm ISSUE-URL`; registry
read/modify/write operations are file-locked against timer and re-arm races.
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

## Configure a recurring campaign

Campaigns are a Home Manager surface because their GitHub producer is a managed
user service. The configured checkout must be writable by that user, have the
named remote, and already have Git and `gh` authentication suitable for pushing,
opening pull requests, and merging them.

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
    mention = "@tally build";
    # The operator posts the mention using this same account's gh token.
    allowSelfTriggered = true;
    allowedActors = [ "mecattaf" ];
    # Public failure comments and stderr publication are independently
    # default-off. Diagnose locally unless this campaign explicitly needs them.
    postFailureEvidence = false;
    postFailureStderr = false;
    worklist = "specs/001-crm/tasks.json";
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
      {
        kind = "command";
        id = "tests";
        preflightArgv = [ "nix" "develop" "--command" "cargo" "--version" ];
        argv = [ "nix" "develop" "--command" "cargo" "test" "--workspace" ];
        runtimeMaxSec = 900;
      }
      {
        kind = "command";
        id = "format";
        preflightArgv = [ "nix" "develop" "--command" "cargo" "fmt" "--version" ];
        argv = [ "nix" "develop" "--command" "cargo" "fmt" "--all" "--check" ];
        runtimeMaxSec = 900;
      }
    ];

    # Held for one bounded reconcile pass, so two triggers cannot mutate this
    # campaign concurrently.
    pool.name = "crm-campaign";
  };
};
```

`allowSelfTriggered` defaults to `false` on the operator-facing mention
producer. Keep that loop-breaker when tally's authenticated GitHub identity is
a bot. Set it to `true` only when the trusted person posting the campaign mention
is also the account authenticated by `gh`, as in the single-account example
above. `allowedActors` filters external actors on both producers;
`allowSelfTriggered` is the separate authorization for the authenticated `gh`
identity and therefore does not require adding that identity to the external
allowlist. The pass-continuation producer matches only the exact continuation
command and always opts into authenticated self-triggering. A pass that merged
work, passed a checkpoint, or published machine steering posts that command
once, without widening external campaign admission.

`postFailureEvidence` posts one comment for each failed attempt, so retries can
accumulate several receipts. `postFailureStderr` requires it and adds only the
bounded, conservatively redacted tail. Redaction cannot recognize every
application secret; leave both defaults off for a public repository unless the
publication policy has been deliberately reviewed. Both the mention and
pass-continuation producers inherit these settings.

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

`forbidPaths` is evaluated against the union of committed, non-deleted paths
changed by every branch commit reachable from the current `HEAD` but not from
the task's prepared base revision. A later deletion does not erase an earlier
forbidden artifact from this history-scoped check. Deletions of artifacts that
were already tracked in the prepared base are ignored, so a task may remove
legacy debris. If a task commits a forbidden artifact, remediation must rewrite
or squash the task branch so the offending commit is no longer reachable;
adding a cleanup commit is intentionally still red.

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
campaign: `producers.campaign-continuation`, an `events-dir` producer that
drains the machine self-continuation every campaign class writes after a pass
merges work, passes a checkpoint, or publishes machine steering. It is not
per-campaign state, so arming still requires no Nix change.

The producer posts its receipt and witnessed evidence. Under the degraded
projection, each merge and passed checkpoint repairs its own worklist checkbox
and a passed checkpoint posts an idempotently marked progress comment; under
the native sub-issue projection neither is written. Once task execution,
integration, and diagnosis settle, a pass that merged work, passed a checkpoint,
or published machine steering writes its continuation payload from one separate
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

`forbidPaths` gates are not preflighted because the unmodified base has no task
history to constrain. They begin in their declared position in the post-agent
gate sequence and use their own `runtimeMaxSec` for the packaged driver node.

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
          "specs/001-crm/spec.md#customer-model"
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
`acceptanceCriteria`, and `dependencies`. `conflictDomains` may be omitted only
while `maxParallel = 1`; every implementation node must provide a non-empty
array when parallelism is enabled. Entries are normalized relative file or
directory paths without `..`. Equal paths and ancestor/descendant paths overlap,
so `src/domain` conflicts with `src/domain/customer.rs`. A reconcile pass
greedily selects ready nodes in worklist order while keeping selected
implementation domains disjoint. Comparisons fold case for portable behavior:
`Docs` also conflicts
with `docs/guide.md`, even when the coordinator's checkout is case-sensitive.
Case-only duplicate declarations are rejected.

A non-empty declaration is also an enforced ownership boundary. Immediately
after the agent exits, before project gates run, a dedicated driver node compares
the union of paths touched by every task commit with the task's domains using
that same case-folded component-prefix rule. A later deletion cannot hide a
transient unowned path. Adds, edits, deletions, type changes, and both sides of a
rename are included. Publication repeats the check against the clean exact head
before the remote branch or pull request can move, and a base-changing rebase
repeats it before force-push. The flow carries whether domains are required into
each enforcing node, so an empty parallel declaration cannot turn enforcement
off. Serial tasks that omit the optional field keep their unrestricted existing
behavior.

Ownership results witness the requirement flag, declared domains, full sorted
owned-path set, base revision, and head. This makes both under-declaration and
unused broad declarations visible in receipts. When enough tasks are ready but
overlapping declarations underfill `maxParallel`, reconciliation emits a
diagnostic naming the blocked tasks and representative overlaps. Shared files
such as changelogs and lockfiles therefore serialize their declaring tasks by
design. There is no append-only exemption: Git still has to reconcile concurrent
content edits, so campaigns that need parallelism should assign those updates to
a dependent consolidation task instead of declaring unsafe sharing.

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
`<campaign>/<task-id>` (for example `crm/customer-model`). It is additive
provenance: the UUID remains the durable identity, while `taskRef` appears in
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
jobs, the sweep may reclaim its worktrees, local branches, task markers, and
pass record.

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

A passed checkpoint is recorded as a lightweight Git tag below
`refs/tags/tally/spec-build/v1/`. The expected ref includes the campaign, issue,
checkpoint ID, full worklist SHA-256, and exact tested base revision. Changing
the declared work graph or advancing the base requires a new pass.
Reconciliation accepts the ref only when it points directly to that named base
commit and every dependency's merge or checkpoint revision is its ancestor.
An older green tag never certifies a later base, even when the later commit is
unrelated to the checkpoint's declared dependencies: checkpoints ask questions
about the accumulated repository state, not only their dependency closure.

Checkpoint refs are immutable and create-only; the driver never force-moves a
receipt. A tag ruleset should allow the tally forge identity to create refs in
this namespace while denying other identities creation and denying updates or
deletion. If protection denies that identity creation, recording fails closed.
The credential allowed to create these refs is itself a trusted completion
authority—Git cannot prove that its holder ran the witnessed command. The
direct-commit, exact-base, and dependency-ancestry checks reject malformed or
inconsistent receipts; namespace protection keeps unrelated push identities
from minting otherwise consistent ones.

Old refs are retained as historical audit receipts. Worklist edits and base
movement make them unreachable from the active completion calculation rather
than deleting them. This deliberately preserves stateless recovery and works
with update/delete-protected tags. When a campaign is permanently
decommissioned, its campaign-and-issue namespace can be pruned under the
repository's ordinary destructive-change procedure; there is no automatic
campaign-lifetime inference or in-run tag garbage collection.

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
  -> post the one marked escalation with accumulated diagnoses -> exit
if implemented is empty, an implementation is in the frontier, and command gates exist:
  prepare an isolated worktree at current remote main
  -> run each command gate.preflightArgv -> clean up the preflight lane
parallel(frontier):
  implementation: prepare isolated worktree -> agent -> witness ownership
    -> each configured gate -> recheck ownership -> push stable task branch
    -> open/reuse PR
  checkpoint: prepare isolated worktree -> run checkpoint argv
    -> record content-and-exact-base-bound completion ref
serial(successful publications): compare current base -> rebase if moved
  -> re-run each configured gate only on a changed rebased head -> merge
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
`taskRef`.

A checkpoint prepares the exact current remote base in its own worktree and
runs its argv as an ordinary settled `campaign-control` node with `exit:0`
evidence, the declared deadline, and the checkpoint's `taskRef`. On success the
driver verifies that `HEAD` is still the prepared base, no tracked file changed,
and the prepared base still belongs to the current remote-base ancestry. It
then publishes an immutable receipt for the exact revision that was tested,
plus a progress comment where the degraded projection is in force. If the
remote base advanced during validation,
the receipt remains truthful historical evidence but is not complete for the
next reconciliation; the checkpoint is prepared again on the newer base. A
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
and does not reread the public channel. tally never changes a running node's
immutable brief. After a task node fails, a separate diagnosis agent receives
four explicit inputs: the failed node's bounded capture stderr, every gate
output collected for the task, the exact task brief, and a bounded diff against
its witnessed base. The diagnosis agent is told not to modify the repository or
repeat secret-looking input. Only its concise output passes through conservative
public redaction and becomes an authenticated, marked campaign comment; raw
capture, gate output, brief, and diff remain private job inputs.

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
there posts a marked retry comment and the continuation instead. That retry
budget is bounded at two per task and is read back from the forge like every
other campaign fact, so a permanently broken lane still spends its two steering
attempts and reaches escalation rather than retrying forever.

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
driver posts one marked escalation containing compact summaries of all machine
diagnoses and never posts it again for that campaign issue. Start investigation
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
same way and neither posts a public self-nudge: a pass that merges, checkpoints,
or diagnoses a failure writes one bounded JSON enqueue payload into the daemon's
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
`tally campaign arm` instead. Promoting a repeated ad-hoc campaign into this
declarative surface is an explicit change of weight class.
