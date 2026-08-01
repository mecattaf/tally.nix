# Campaigns

`services.tally.campaigns` turns a frozen specification corpus into a
forge-reconciled build campaign. The spec repository defines the work, merged
pull requests are its durable checkboxes, one GitHub issue admits and steers
reconcile passes, and tally witnesses every observation and gate. A new campaign
needs no new flow or producer code.

Keep those roles separate:

- **The spec repository is the work source.** Its versioned tasks artifact
  contains the decomposition and each complete per-task brief.
- **GitHub is intake, steering, state, and projection.** An exact mention on one
  open, labeled campaign issue starts a pass. Merged task pull requests are the
  completion facts read by every later pass. Issue comments steer later agent
  attempts; receipts and evidence project each reconciliation.
- **tally is the workflow engine.** It validates and witnesses the worklist,
  selects the dependency-ready frontier, creates isolated worktrees, runs
  deterministic gates, and serializes re-gated merges.

The module deliberately supplies mechanism, not project policy. Repository
owners still choose the corpus shape, gates, adapter, label, trusted actors, and
when a corpus is frozen.

## Configure one campaign

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
    # implementation node is writable and may request bounded escalation.
    agentApprovalPolicy = "on-request";
    agentSandboxPolicy = "workspace-write";
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
above. `allowedActors` applies to both producers. The separately rendered
merge-continuation producer matches only the exact continuation command and
allows authenticated self-triggering, but retains that same actor allowlist.
When `allowedActors` is non-empty, include the `gh` identity so its merge
comments can start the next pass. One merged task therefore creates one
deduplicated next-pass event without widening campaign admission.

`postFailureEvidence` posts one comment for each failed attempt, so retries can
accumulate several receipts. `postFailureStderr` requires it and adds only the
bounded, conservatively redacted tail. Redaction cannot recognize every
application secret; leave both defaults off for a public repository unless the
publication policy has been deliberately reviewed. Both the mention and
merge-continuation producers inherit these settings.

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
transport.

An implementation node defaults to `agentSandboxPolicy = "workspace-write"`
because its contract requires a commit, paired with
`agentApprovalPolicy = "on-request"` so the adapter can surface a request to go
beyond that boundary. Both names must exist in the selected adapter's launch
maps; deployment fails early otherwise. Set `agentSandboxPolicy = "read-only"`
when a deliberately non-writing campaign agent needs that constraint. Set either
option to `null` only for an adapter such as the shell fixture that declares no
corresponding policy map.

One enabled attrset expands to all of the following:

| Rendered mechanism | Contract |
|---|---|
| `flows.<name>` | The content-addressed shipped `spec-build` script, bounded to one `maxParallel` frontier and its gates. |
| `producers.campaign-<name>` | A GitHub search producer scoped to the configured repositories, open issues, label, exact mention, and optional actor allowlist. |
| `producers.campaign-<name>-reconcile` | A GitHub search producer for the exact self-posted continuation command emitted after a merge. |
| `<pool.name>` | A capacity-1 mutex held for one reconcile pass. |
| `campaign-agent` | A counted `slot` pool whose rendered capacity is the largest enabled `maxParallel`. |
| `campaign-control` | A small `cpu-slot` pool for reconciliation, Git, GitHub, and gate nodes. |
| `spec-build-driver` | The packaged deterministic policy driver used for reconcile, prep, built-in constraints, publish, rebase, and merge projections. |

The producer posts its receipt and witnessed evidence. Each merge posts an
idempotently marked progress comment plus the exact continuation command; the
next poll admits a fresh pass behind the campaign mutex. Neither producer closes
the campaign issue. It remains the durable steering channel across passes.

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

## The per-task brief contract

`worklist` is a relative glob beneath the configured checkout. It must resolve
to exactly one regular JSON file and may not contain `..`. The shipped driver
accepts schema version 1:

```json
{
  "schemaVersion": 1,
  "tasks": [
    {
      "id": "customer-model",
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
    }
  ]
}
```

Every task requires the first seven fields shown above. `conflictDomains` may be
omitted only while `maxParallel = 1`; every task must provide a non-empty array
when parallelism is enabled. Entries are normalized relative file or directory
paths without `..`. Equal paths and ancestor/descendant paths overlap, so
`src/domain` conflicts with `src/domain/customer.rs`. A reconcile pass greedily
selects ready tasks in worklist order while keeping the selected domains
disjoint.

A non-empty declaration is also an enforced ownership boundary. Before pushing
a task branch, the driver compares every path in its committed base-to-head diff
with the task's domains using that same component-prefix rule. An unowned add,
edit, deletion, type change, or either side of a rename fails publication before
the remote branch or pull request can move. A base-changing rebase repeats the
check against the rewritten exact head before force-push. Serial tasks that omit
the optional field keep their unrestricted existing behavior.

IDs are stable task components. Dependencies must name earlier tasks, which
makes the array a validated topological order. Acceptance criteria are runnable,
direct-argv instructions for the agent and reviewers; the campaign's configured
`gates` remain the independent merge criterion executed by tally.

Every task-specific node also carries the campaign-scoped reference
`<campaign>/<task-id>` (for example `crm/customer-model`). It is additive
provenance: the UUID remains the durable identity, while `taskRef` appears in
node receipts, lifecycle and query output, `TALLY_TASK_REF`, unit names, and
capture names. The worklist discovery node has no task ID and therefore no
`taskRef`.

The first node of every pass parses, normalizes, schema-validates, and witnesses
this artifact together with its relative path and SHA-256 digest. The same node
queries merged pull requests carrying tally's campaign/task marker, subtracts
those task IDs, applies `dependencies ⊆ merged`, and selects at most
`maxParallel` conflict-disjoint tasks. Later nodes use only that witnessed
result. The implementation node receives its one task, assigned workspace,
campaign issue locator, and bounded mission. It is explicitly told not to read
another task from the worklist and not to push, open a pull request, or merge.
Those are separate deterministic nodes.

## Reconciliation, parallelism, and the merge criterion

One invocation is one bounded reconcile pass:

```text
merged = marked merged PRs
remaining = worklist - merged
ready = tasks in remaining whose dependencies are all in merged
frontier = first maxParallel ready tasks with disjoint conflictDomains

if merged is empty and command gates exist:
  prepare an isolated worktree at current remote main
  -> run each command gate.preflightArgv -> clean up the preflight lane
parallel(frontier): prepare isolated worktree -> agent -> each configured gate
  -> push stable task branch -> open/reuse PR
serial(successful publications): compare current base -> rebase if moved
  -> re-run each configured gate only on a changed rebased head -> merge
exit
```

Until the first marked campaign pull request is merged, every fresh pass with a
command gate runs the preflight on a separate pristine-base worktree before
admitting any agent. Each command gate explicitly separates a base-safe
`preflightArgv` from its post-change merge-criterion `argv`. Preflight uses the
first frontier task's environment, the same execution host and deadline as the
post-change gate, and a lane that is cleaned before dispatch. The first merged
task is durable forge proof that campaign admission passed; later passes do not
repeat preflight.
Because it validates the first frontier task's prepared environment, each
preflight node carries that task's `taskRef`.

The agent must leave a clean worktree with at least one commit descended from
the prepared base. Publication refuses dirty, empty, or non-descendant work.
Each task has a stable remote branch across passes and a run-local worktree lane,
so a dead runner cannot make a later pass share a writable directory with an
old child.

Publications may finish in parallel, but integration follows deterministic
frontier order. Before each merge the driver fetches current base. If the
already-gated head contains it, integration is a no-op. If base moved, the
driver rebases with an exact force-with-lease, tally re-runs every configured
gate on that new head, and merge refuses if either base or task branch moved
again. Thus concurrent implementation does not weaken “witnessed gates are the
merge criterion." A dependent task cannot enter any frontier until its
prerequisite PR is observed merged by a later pass.

A preflight failure stops the pass before any agent is admitted. Agent, task
gate, publication, rebase, and merge failures are settled into the pass report.
A failed task remains unmerged and therefore eligible for a later pass;
successful conflict-disjoint siblings still publish and merge.

## Failure, steering, and re-entry

Use the campaign issue comments for steering. Each new agent attempt reads that
channel before changing code. tally never changes a running node's immutable
brief.

After a task failure:

1. Inspect `tally query log` and the local structured flow error, which carries
   the failed child and that child's bounded captured tail. A public campaign
   receipt is absent by default; it includes failure metadata only with
   `postFailureEvidence` and a conservatively redacted tail only with the
   additional `postFailureStderr` opt-in. Locate the full
   lifecycle with `tally query log --flow-run <runner-task-uuid>` when needed.
   Task-specific records expose `taskRef`, so the worklist ID is visible
   without a UUID lookup.
2. Add the steering decision to the campaign issue.
3. Correct any host or base defect exposed by preflight, then post the
   configured mention again:

   ```console
   $ gh issue comment ISSUE --repo OWNER/REPO --body '<configured mention>'
   ```

That fresh event creates a fresh flow-run identity. The pass does not reuse or
repair an old runner prefix: it observes merged PRs again, recomputes the whole
frontier, and gives the failed task a new isolated lane with current steering.
Changing campaign arguments or deploying a new content-addressed script between
passes is therefore ordinary generation change, not replay divergence. Duplicate
mentions are safe because the campaign mutex serializes passes and each pass
subtracts the same forge facts before dispatch.

Each pass contains at most one bounded frontier, so the fixed 24-hour evaluator
budget no longer measures the whole campaign. If a pass process dies, wait for
any admitted children to settle and post a fresh mention. Stable remote task
branches preserve published work; merged PRs preserve completion. Generic flows
that truly require one run identity still use [submission identity and
replay](submission-and-replay.md).

## Starting a new repository

The complete operational sequence is:

1. Freeze and commit the spec corpus, including its schema-versioned task
   artifact and style-transfer references.
2. Provision its writable checkout and adapter authentication on the tally
   host.
3. Open one issue in that repository with the configured label.
4. Add one `services.tally.campaigns.<name>` attrset and deploy Home Manager.
5. Run `tally adapter smoke <agent>` on the deployed host.
6. Post the exact configured mention on the issue.

That is the whole activation path: no per-repository flow script, dispatch
wrapper, producer block, or extra serialization service.
