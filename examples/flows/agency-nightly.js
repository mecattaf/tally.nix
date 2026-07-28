// The agency nightly wave.
//
// One overnight increment, start to finish, with no human in the middle:
//
//   worklist (deterministic)
//     -> implement-<taskId>  (codex, one worktree/branch/key per task, parallel)
//     -> review-<taskId>     (claude-code, cross-harness, parallel, never certifies)
//     -> culminate (deterministic: push, open pull requests, write a morning report)
//
// WHERE THE WORKLIST COMES FROM (ruling, 2026-07-27)
// -------------------------------------------------
// The worklist IS this script plus its `args`. There is no external worklist
// contract to satisfy: no tasks.md adapter, no labelled-issue parser, no
// scraping of an ambient queue. `args.wave` is the wave, declared in
// `services.tally.flows.agency-nightly.args`, checked against `meta.argsSchema`
// when the generation is built, and pinned per run by `argsHash`. Defining a new
// wave means regenerating this flow's arguments -- which is a Nix change, seen
// by review, and reproducible from the store path.
//
// The first node is still deterministic and still *witnesses* the wave: it
// resolves the base revision to a commit id and materialises one git worktree
// and branch per task. Every downstream node key derives from that witnessed
// result rather than from `args`, so replay keys track what the run actually
// prepared.
//
// CONTENTION CONTROL IS PRIORITY, NOT A BUDGET (ruling, 2026-07-27)
// -----------------------------------------------------------------
// Flows are excluded from windowed-consumption admission by design (#142), and
// `budgetPool` no longer exists. Every node here runs at `priority: "low"` so
// that anything queued before bed runs first and a more important ask can
// intercede mid-wave. That is the whole mechanism; there is no window
// staggering and no per-node consumption estimate.
//
// Consequently `codex-window` and `claude-window` must be ordinary pools in the
// operator's configuration. A windowed-consumption predicate on either name is
// rejected at `tally flow check` time with `windowed-consumption-excluded`,
// before activation.
//
// THE THREE CAPS
// --------------
// * `meta.maxNodes` = 20 bounds non-deleted rows created for one flow-run
//   identity, for the run's lifetime. Nodes that finish normally free nothing.
//   A full wave creates 1 worklist + 6 implement + 6 review + 1 culminate = 14
//   rows, leaving 6 rows of deliberate repair headroom: enough to re-dispatch
//   every task in the wave once under a new key without exhausting the cap.
//   The configured `maxNodes` may be larger, never smaller; the smaller
//   applicable bound governs at runtime.
// * `meta.iterationCap` = 8 bounds how many nodes a single call site may create.
//   The `codex()` site runs once per task and the `claude()` site once per
//   task, so `maxWaveSize` of 6 is the real load and 8 is the backstop. The
//   shared `job()` site inside driverNode() runs exactly twice.
// * `guardrails.fanoutCap` (kernel configuration, default 64) bounds the
//   children one parent may have outstanding at a time. The runner is the
//   parent of every node here and its widest moment is the wave itself, so peak
//   outstanding children is `maxWaveSize` = 6. A wave wider than the configured
//   fanout cap would be refused at admission, not queued; that is the ceiling
//   `maxWaveSize` is capped at 6 to stay far below.
//
// THE RUNNER'S LIFETIME
// ---------------------
// The runner holds one `flow` slot for the whole wave. Work time is bounded by
//
//   2 * driver.runtimeMaxSec            (worklist + culminate, serial)
//   + implementationRuntimeMaxSec       (the slowest implementation)
//   + reviewRuntimeMaxSec               (its review)
//
// because the per-task chains run concurrently, so the wave costs one task's
// depth rather than the sum. With the documented example arguments that is
// 2*900 + 14400 + 7200 = 23400s, comfortably inside the 43200s configured
// default for `services.tally.flows.<name>.runtimeMaxSec`.
//
// That arithmetic bounds *work*, not queue wait: these are low-priority nodes
// and may be starved arbitrarily long by design. If starvation pushes the
// runner past its watchdog the job ends `RuntimeExceeded`, which is exactly the
// verdict an automatic bounded requeue acts on -- see below.
//
// CRASH RESUMPTION
// ----------------
// The flow-run identity is the runner job's task UUID, and that UUID survives a
// retry: `queue.retry` bumps `attempt` and keeps `task_uuid`. So a second
// invocation of the runner re-enters the same run, reuses every node whose key
// already reached a terminal witness, and attaches to the one still in flight.
// Nothing in this script has to know it is a replay.
//
// Automatic, no operator action: `RuntimeExceeded` (with
// `recoveryPolicy.retry.autoBoundedRequeue`), `Preempted` (`autoResourceReturn`)
// and `PoolVanished` (`autoPoolReturn`) requeue the same task UUID.
//
// NOT automatic: an ordinary crash -- nonzero exit, panic, OOM kill -- lands
// `Verdict::Failed`, which carries no retry trigger at all and is therefore
// ineligible for any automatic policy. Resuming that wave is one operator
// command against the runner's task UUID:
//
//     tally queue retry <runner-task-uuid>
//
// A scheduler-side trigger that re-invokes an existing flowRunId on a timer,
// which would close the gap without a clock inside the flow, is filed as a
// separate design issue rather than improvised here.
export const meta = {
  name: "agency-nightly",
  description: "Run one overnight agency wave: implement, cross-review, culminate",
  pools: ["agency-control", "codex-window", "claude-window"],
  argsSchema: {
    type: "object",
    required: [
      "repository",
      "checkout",
      "baseRev",
      "baseBranch",
      "worktreeRoot",
      "branchPrefix",
      "reportPath",
      "driver",
      "implementationRuntimeMaxSec",
      "reviewRuntimeMaxSec",
      "wave"
    ],
    properties: {
      repository: { type: "string", pattern: "^[^/ \t]+/[^/ \t]+$" },
      checkout: { type: "string", pattern: "^/" },
      baseRev: { type: "string", minLength: 1 },
      baseBranch: { type: "string", pattern: "^[A-Za-z0-9._/+-]+$" },
      worktreeRoot: { type: "string", pattern: "^/" },
      branchPrefix: { type: "string", pattern: "^[A-Za-z0-9._/-]+$" },
      reportPath: { type: "string", pattern: "^/" },
      driver: {
        type: "object",
        required: ["adapter", "program", "runtimeMaxSec"],
        properties: {
          adapter: { type: "string", minLength: 1 },
          program: { type: "string", pattern: "^/" },
          runtimeMaxSec: { type: "integer", minimum: 1 }
        },
        additionalProperties: false
      },
      implementationRuntimeMaxSec: { type: "integer", minimum: 1 },
      reviewRuntimeMaxSec: { type: "integer", minimum: 1 },
      wave: {
        type: "array",
        minItems: 1,
        maxItems: 6,
        items: {
          type: "object",
          required: ["taskId", "title", "mission", "acceptanceCriteria"],
          properties: {
            taskId: {
              type: "string",
              pattern: "^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$",
              maxLength: 40
            },
            title: { type: "string", minLength: 1, maxLength: 200 },
            mission: { type: "string", minLength: 1, maxLength: 8000 },
            acceptanceCriteria: {
              type: "array",
              minItems: 1,
              maxItems: 20,
              items: { type: "string", minLength: 1, maxLength: 2000 }
            },
            issue: { type: "string", pattern: "^[1-9][0-9]*$" }
          },
          additionalProperties: false
        }
      }
    },
    additionalProperties: false
  },
  maxNodes: 20,
  iterationCap: 8,
  selectors: []
};

// The wave never grows past this, so the node budget above stays true whatever
// `args.wave` says. `meta.argsSchema` enforces the same number.
const maxWaveSize = 6;

const taskIdSchema = {
  type: "string",
  pattern: "^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$",
  maxLength: 40
};

const commitSchema = { type: "string", pattern: "^[0-9a-f]{40,64}$" };

const taskSchema = {
  type: "object",
  required: ["taskId", "title", "mission", "acceptanceCriteria"],
  properties: {
    taskId: taskIdSchema,
    title: { type: "string", minLength: 1 },
    mission: { type: "string", minLength: 1 },
    acceptanceCriteria: {
      type: "array",
      minItems: 1,
      items: { type: "string", minLength: 1 }
    },
    issue: { type: "string", pattern: "^[1-9][0-9]*$" }
  },
  additionalProperties: false
};

const workspaceSchema = {
  type: "object",
  required: ["taskId", "branch", "worktreePath"],
  properties: {
    taskId: taskIdSchema,
    branch: { type: "string", minLength: 1 },
    worktreePath: { type: "string", pattern: "^/" }
  },
  additionalProperties: false
};

// What the deterministic first node witnesses: the wave it actually prepared,
// the commit it pinned, and the worktree it cut for each task.
const worklistSchema = {
  type: "object",
  required: ["schemaVersion", "repository", "baseRev", "tasks", "workspaces"],
  properties: {
    schemaVersion: { const: 1 },
    repository: { type: "string", minLength: 1 },
    baseRev: commitSchema,
    tasks: {
      type: "array",
      minItems: 1,
      maxItems: maxWaveSize,
      items: taskSchema
    },
    workspaces: {
      type: "array",
      minItems: 1,
      maxItems: maxWaveSize,
      items: workspaceSchema
    }
  },
  additionalProperties: false
};

const implementationSchema = {
  type: "object",
  required: ["taskId", "branch", "head", "summary", "tests"],
  properties: {
    taskId: taskIdSchema,
    branch: { type: "string", minLength: 1 },
    head: commitSchema,
    summary: { type: "string", minLength: 1, maxLength: 12000 },
    tests: {
      type: "array",
      items: { type: "string", minLength: 1, maxLength: 2000 }
    }
  },
  additionalProperties: false
};

const reviewSchema = {
  type: "object",
  required: ["taskId", "reviewedHead", "verdict", "summary", "findings"],
  properties: {
    taskId: taskIdSchema,
    reviewedHead: commitSchema,
    verdict: { enum: ["approve", "changes-requested"] },
    summary: { type: "string", minLength: 1, maxLength: 12000 },
    findings: {
      type: "array",
      items: {
        type: "object",
        required: ["severity", "text"],
        properties: {
          severity: { enum: ["blocking", "non-blocking"] },
          text: { type: "string", minLength: 1, maxLength: 4000 }
        },
        additionalProperties: false
      }
    }
  },
  additionalProperties: false
};

const culminationSchema = {
  type: "object",
  required: ["status", "reportPath", "pullRequests", "failures"],
  properties: {
    status: { enum: ["ready", "partial", "worklist-failed"] },
    reportPath: { type: "string", pattern: "^/" },
    pullRequests: {
      type: "array",
      maxItems: maxWaveSize,
      items: {
        type: "object",
        required: ["taskId", "branch", "status", "url"],
        properties: {
          taskId: taskIdSchema,
          branch: { type: "string", minLength: 1 },
          status: { enum: ["created", "existing", "no-changes"] },
          url: { type: ["string", "null"] }
        },
        additionalProperties: false
      }
    },
    failures: {
      type: "array",
      maxItems: maxWaveSize,
      items: {
        type: "object",
        required: ["taskId", "stage", "code", "message"],
        properties: {
          taskId: taskIdSchema,
          stage: { enum: ["implementation", "review"] },
          code: { type: "string", minLength: 1 },
          message: { type: "string", minLength: 1 }
        },
        additionalProperties: false
      }
    }
  },
  additionalProperties: false
};

// The deterministic driver reports domain failures as data, inside `exit:0`,
// rather than by dying. A thrown error would classify as a script bug and cost
// the culmination; a discriminated envelope validated by resultSchema keeps the
// failure typed, witnessed, and survivable.
function envelopeSchema(valueSchema) {
  return {
    oneOf: [
      {
        type: "object",
        required: ["ok", "value"],
        properties: { ok: { const: true }, value: valueSchema },
        additionalProperties: false
      },
      {
        type: "object",
        required: ["ok", "error"],
        properties: {
          ok: { const: false },
          error: {
            type: "object",
            required: ["code", "message"],
            properties: {
              code: { type: "string", minLength: 1 },
              message: { type: "string", minLength: 1 },
              details: { type: "object" }
            },
            additionalProperties: false
          }
        },
        additionalProperties: false
      }
    ]
  };
}

function bounded(value, limit) {
  const text = typeof value === "string" ? value : JSON.stringify(value);
  if (typeof text !== "string" || text.length === 0) {
    return "(no detail)";
  }
  return text.length > limit ? `${text.slice(0, limit)}...` : text;
}

// A settled node resolves with its NodeResult whatever the verdict, so a failure
// arrives as data: the verdict the daemon witnessed, plus whatever detail
// survived.
//
// The verdict leads, not `error.code`. A node that failed before producing a
// structured result also fails its resultSchema, and that check replaces
// `error` wholesale -- so a failed node reaches the script reporting
// `result-schema-mismatch` / "node returned no structured result", which is the
// symptom rather than the event. The daemon's own message is not lost, it is in
// the witness record; it just is not what the flow sees. Reading the verdict
// first is what keeps the morning report honest about what happened.
function nodeError(node) {
  const detail = (node.error && node.error.message) || "no structured result";
  return {
    code:
      node.verdict === "pass"
        ? (node.error && node.error.code) || "node-result-invalid"
        : `node-${node.verdict}`,
    message: bounded(`${node.verdict}: ${detail}`, 2000)
  };
}

function nodeFailure(taskId, stage, node) {
  const error = nodeError(node);
  return { taskId, stage, code: error.code, message: error.message };
}

function driverNode(action, brief, resultSchema, evidence, settle) {
  return job(
    {
      argv: [args.driver.program, action],
      adapter: args.driver.adapter,
      pools: ["agency-control"],
      priority: "low",
      runtimeMaxSec: args.driver.runtimeMaxSec,
      evidence,
      brief,
      key: action,
      label: `agency-${action}`,
      resultSchema: envelopeSchema(resultSchema)
    },
    { settle }
  );
}

function implementationPrompt(task, workspace, baseRev) {
  return [
    `Implement agency wave task "${task.taskId}": ${task.title}`,
    `Repository: ${args.repository}`,
    `Pinned base revision: ${baseRev}`,
    `Branch: ${workspace.branch}`,
    `Worktree: ${workspace.worktreePath}`,
    "",
    task.mission,
    "",
    `Acceptance criteria: ${JSON.stringify(task.acceptanceCriteria)}`,
    "Work only inside the assigned worktree, on the assigned branch. Implement the",
    "complete task, run proportionate tests, and commit the result.",
    "Do not push, and do not create or merge a pull request: the deterministic",
    "culmination owns both, so that what reaches GitHub is what was witnessed.",
    `Return only JSON matching {"taskId":"${task.taskId}","branch":"${workspace.branch}",`,
    '"head":"<40-or-64-hex-commit>","summary":"<bounded summary>",',
    '"tests":["<command and outcome>"]}.'
  ].join("\n");
}

function reviewPrompt(task, workspace, implementation) {
  return [
    `Independently review agency wave task "${task.taskId}": ${task.title}`,
    `Repository: ${args.repository}`,
    `Branch: ${workspace.branch}`,
    `Worktree: ${workspace.worktreePath}`,
    `Commit under review: ${implementation.head}`,
    "",
    `Acceptance criteria: ${JSON.stringify(task.acceptanceCriteria)}`,
    `Implementation report: ${JSON.stringify(implementation)}`,
    "",
    "Read the committed diff and run whatever checks the acceptance criteria need.",
    "Do not modify the worktree, the branch, the issue, or any pull request.",
    "You are a finder, not a certifier: a different harness wrote this code, and",
    "your verdict is evidence for the human culmination rather than a gate. The",
    "pull request is opened either way, carrying whatever you find.",
    `Return only JSON matching {"taskId":"${task.taskId}",`,
    `"reviewedHead":"${implementation.head}","verdict":"approve|changes-requested",`,
    '"summary":"<bounded review>",',
    '"findings":[{"severity":"blocking|non-blocking","text":"<finding>"}]}.'
  ].join("\n");
}

(async () => {
  const worklistNode = await driverNode(
    "worklist",
    {
      action: "worklist",
      repository: args.repository,
      checkout: args.checkout,
      baseRev: args.baseRev,
      baseBranch: args.baseBranch,
      worktreeRoot: args.worktreeRoot,
      branchPrefix: args.branchPrefix,
      wave: args.wave
    },
    worklistSchema,
    ["exit:0"],
    // Settled: a worklist that cannot prepare the wave still owes the operator a
    // morning report saying so, so this failure routes to the culmination rather
    // than ending the run.
    true
  );

  const worklist =
    worklistNode.verdict === "pass" && worklistNode.result && worklistNode.result.ok
      ? worklistNode.result.value
      : null;

  // Node keys come from the witnessed worklist, never from args: if the driver
  // prepared a narrower wave than was declared, the run follows what exists on
  // disk. A task the driver listed but did not cut a worktree for is dropped
  // here and reported as a failure rather than dereferenced.
  const prepared = worklist
    ? worklist.tasks.map(task => ({
        task,
        workspace: worklist.workspaces.find(entry => entry.taskId === task.taskId)
      }))
    : [];
  const tasks = prepared.filter(entry => entry.workspace);
  const unprepared = prepared.filter(entry => !entry.workspace);

  const workspaceFor = entry => ({
    repo: args.repository,
    baseRev: worklist.baseRev,
    branch: entry.workspace.branch,
    worktreePath: entry.workspace.worktreePath
  });

  // pipeline() rather than two parallel() phases: each task's review starts the
  // moment its own implementation lands, so no task waits on the slowest peer.
  // `settle: true` on the combinator is the second belt -- per-node settle
  // already turns a failed task into data, and this catches everything else
  // (submission refusal, cap exhaustion) so that one bad task can never suppress
  // the culmination. The culmination is the gate; it must always run.
  const outcomes = await pipeline(
    tasks,
    async entry => {
      const implementation = await codex(
        implementationPrompt(entry.task, entry.workspace, worklist.baseRev),
        {
          priority: "low",
          runtimeMaxSec: args.implementationRuntimeMaxSec,
          workspace: workspaceFor(entry),
          key: `implement-${entry.task.taskId}`,
          label: `implement-${entry.task.taskId}`,
          resultSchema: implementationSchema,
          settle: true
        }
      );
      if (implementation.verdict !== "pass" || !implementation.result) {
        return {
          taskId: entry.task.taskId,
          failure: nodeFailure(entry.task.taskId, "implementation", implementation)
        };
      }
      return { taskId: entry.task.taskId, implementation: implementation.result };
    },
    async (stage, entry) => {
      if (stage.failure) {
        return stage;
      }
      const review = await claude(
        reviewPrompt(entry.task, entry.workspace, stage.implementation),
        {
          priority: "low",
          runtimeMaxSec: args.reviewRuntimeMaxSec,
          workspace: workspaceFor(entry),
          key: `review-${entry.task.taskId}`,
          label: `review-${entry.task.taskId}`,
          resultSchema: reviewSchema,
          settle: true
        }
      );
      if (review.verdict !== "pass" || !review.result) {
        return {
          taskId: stage.taskId,
          implementation: stage.implementation,
          failure: nodeFailure(entry.task.taskId, "review", review)
        };
      }
      return {
        taskId: stage.taskId,
        implementation: stage.implementation,
        review: review.result
      };
    },
    { settle: true }
  );

  // Outcomes are index-aligned with `tasks`, so a chain that failed outside the
  // settled nodes still names its task.
  const culminationTasks = outcomes
    .map((outcome, index) => {
      const entry = tasks[index];
      if (outcome.ok) {
        return {
          task: entry.task,
          workspace: entry.workspace,
          implementation: outcome.value.implementation || null,
          review: outcome.value.review || null,
          failure: outcome.value.failure || null
        };
      }
      const error = outcome.error || {};
      return {
        task: entry.task,
        workspace: entry.workspace,
        implementation: null,
        review: null,
        failure: {
          taskId: entry.task.taskId,
          stage: "implementation",
          code: bounded(error.code || "flow-chain-failed", 200),
          message: bounded(error.message || "the task chain failed", 2000)
        }
      };
    })
    .concat(
      unprepared.map(entry => ({
        task: entry.task,
        workspace: null,
        implementation: null,
        review: null,
        failure: {
          taskId: entry.task.taskId,
          stage: "implementation",
          code: "worklist-workspace-missing",
          message: "the worklist named this task but prepared no worktree for it"
        }
      }))
    );

  const culminationNode = await driverNode(
    "culminate",
    {
      action: "culminate",
      repository: args.repository,
      checkout: args.checkout,
      baseBranch: args.baseBranch,
      baseRev: worklist ? worklist.baseRev : null,
      reportPath: args.reportPath,
      // The driver's own envelope when it reported one, the node's verdict when
      // the driver never got that far.
      worklistError: worklist
        ? null
        : worklistNode.result && worklistNode.result.error
          ? {
              code: worklistNode.result.error.code,
              message: bounded(worklistNode.result.error.message, 2000)
            }
          : nodeError(worklistNode),
      tasks: culminationTasks
    },
    culminationSchema,
    ["exit:0", `artifact:${args.reportPath}`, "hash:sha256"],
    // Deliberately NOT settled. Everything above is survivable; the culmination
    // is not. If the morning report does not exist the operator wakes to
    // nothing, and that is the one outcome worth ending the run over. Leaving
    // this node unsettled means the engine raises its own FlowTerminalError,
    // carrying the node record, instead of the script inventing an error.
    false
  );

  // The single deliberate throw in this script. The node itself passed, so the
  // engine has nothing to object to, but the driver reported that it could not
  // produce the report -- and there is no later stage left to carry that. Naming
  // the error gives it a stable `name`/`code` in the runner's failure rather
  // than the generic script-bug classification.
  if (!culminationNode.result.ok) {
    const failure = new Error(
      `the agency culmination reported a failure: ${bounded(
        culminationNode.result.error.message,
        2000
      )}`
    );
    failure.name = "AgencyCulminationError";
    failure.code = culminationNode.result.error.code;
    throw failure;
  }

  return {
    repository: args.repository,
    baseRev: worklist ? worklist.baseRev : null,
    wave: tasks.map(entry => entry.task.taskId),
    culmination: culminationNode.result.value
  };
})();
