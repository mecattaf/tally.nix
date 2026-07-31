# Campaigns

`services.tally.campaigns` turns a frozen specification corpus into an ordered
build campaign. The spec repository defines the work, one GitHub issue admits
and steers a run, and tally witnesses the workflow. A new campaign needs no new
flow or producer code.

Keep those roles separate:

- **The spec repository is the work source.** Its versioned tasks artifact
  contains the decomposition and each complete per-task brief.
- **GitHub is intake, steering, and projection.** An exact mention on one open,
  labeled campaign issue starts a run. Issue comments steer later agent
  attempts. Receipts, evidence, and per-task pull requests project progress.
- **tally is the workflow engine.** It validates and witnesses the worklist,
  creates each worktree, delivers one task, runs deterministic gates, and
  publishes and merges in order.

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
    allowedActors = [ "mecattaf" ];
    worklist = "specs/001-crm/tasks.json";
    maxTasks = 32;

    agent = "codex";
    gates = [
      {
        id = "tests";
        argv = [ "nix" "develop" "--command" "cargo" "test" "--workspace" ];
      }
      {
        id = "format";
        argv = [ "nix" "develop" "--command" "cargo" "fmt" "--all" "--check" ];
      }
    ];

    # Held by the runner for the whole campaign, so two mentions cannot
    # advance this campaign concurrently.
    pool.name = "crm-campaign";
  };
};
```

Gate commands are direct argv, not shell strings. Use `sh -c` explicitly only
when shell syntax is actually part of project policy. `agentArgv` normally stays
at its default: a fixed instruction telling the adapter to read the structured
brief at `TALLY_BRIEF`. It can be overridden for a fixture or a purpose-built
adapter executable, but the campaign never interpolates task prose into argv.

One enabled attrset expands to all of the following:

| Rendered mechanism | Contract |
|---|---|
| `flows.<name>` | The content-addressed shipped `spec-build` script, bounded from `maxTasks` and the gate count. |
| `producers.campaign-<name>` | A GitHub search producer scoped to the configured repositories, open issues, label, exact mention, and optional actor allowlist. |
| `<pool.name>` | A capacity-1 mutex held by the runner process. |
| `campaign-agent` | A capacity-1 `slot` pool for implementation agents. |
| `campaign-control` | A small `cpu-slot` pool for worklist, Git, GitHub, and gate nodes. |
| `spec-build-driver` | The packaged deterministic policy driver used for worklist, prep, publish, and merge projections. |

The producer posts its receipt and witnessed evidence, and each merge posts one
idempotently marked progress comment. It does not close the campaign issue on
either pass or acceptance. The issue remains the durable steering channel
across runs.

Before admitting the first real run after deployment, verify the selected
implementation adapter on that host:

```console
$ tally adapter smoke codex
```

That command is the activation check introduced by issue #233; campaign
rendering does not depend on its implementation.

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
      "dependencies": []
    }
  ]
}
```

Every task requires all seven fields shown above. IDs are stable node-key
components. Dependencies must name earlier tasks, which makes the array a
validated topological order. Acceptance criteria are runnable, direct-argv
instructions for the agent and reviewers; the campaign's configured `gates`
remain the independent merge criterion executed by tally.

The first flow node parses, normalizes, schema-validates, and witnesses this
artifact together with its relative path and SHA-256 digest. Later nodes use
only that result. The implementation node receives a structured brief containing
its own task, assigned workspace, campaign issue locator, and a bounded mission.
It is explicitly told not to read another task from the worklist and not to
push, open a pull request, or merge. Those are separate deterministic nodes.

## Ordering and merge criterion

For every witnessed task, `spec-build` executes this chain serially:

```text
fetch current remote main -> prepare worktree -> agent -> configured gates
  -> push branch -> open/reuse PR -> merge -> fetch current main for next task
```

The agent must leave a clean worktree with at least one commit descended from
the prepared base. Publication refuses dirty, empty, or non-descendant work.
The merge is witnessed before the next prep node is submitted. Consequently,
task 2 is prepared from a remote base that already contains task 1's merge;
declaring a dependency is not merely descriptive.

The first non-passing agent, gate, publish, or merge node stops JavaScript
evaluation. No later gate, pull request, merge, or task prep is admitted.

## Failure, steering, and replay

Use the campaign issue comments for steering. Each new agent attempt is told to
read that channel before changing code. tally never treats a comment as new work
and never changes a running node's immutable brief.

After a non-passing node:

1. Inspect the campaign receipt/evidence and locate the failed node, for example
   with `tally query log --flow-run <runner-task-uuid>`.
2. Add the steering decision to the campaign issue.
3. Correct the failed frontier. Retry a failed agent node with
   `tally queue retry <agent-task-uuid>`; its new attempt reads the comments.
   For a red deterministic gate, correct and commit the existing worktree, then
   retry that failed gate with `tally queue retry <gate-task-uuid>`. A passing
   agent node cannot be retried implicitly, and tally does not invent a repair
   attempt after a red gate.
4. Retry the failed runner job:

   ```console
   $ tally queue retry <runner-task-uuid>
   ```

The runner task UUID is the flow-run identity. Its new attempt re-executes the
same content-addressed script with the same arguments: passing prefix nodes are
`reused`, the explicitly retried frontier now projects its latest witness, and
only the next node is `created`. A non-pass witness is never retried merely by
restarting the runner.

Campaigns longer than the fixed 24-hour evaluator budget use the same mechanism.
The budget exit is a continuation boundary, not cancellation: retry the runner
job and tally reconstructs state from durable node witnesses. The full identity,
frontier, and divergence rules are in [Submission identity and
replay](submission-and-replay.md#continuation-after-budget-exhaustion).

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
