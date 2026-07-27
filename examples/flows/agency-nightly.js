export const meta = {
  name: "agency-nightly",
  description: "Implement and cross-review the next labeled GitHub issue wave",
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
      "maxWaveSize",
      "reportPath",
      "driver"
    ],
    properties: {
      repository: {
        type: "string",
        pattern: "^[^/[:space:]]+/[^/[:space:]]+$"
      },
      checkout: { type: "string", pattern: "^/" },
      baseRev: { type: "string", minLength: 1 },
      baseBranch: {
        type: "string",
        pattern: "^[A-Za-z0-9._/+-]+$",
        minLength: 1
      },
      worktreeRoot: { type: "string", pattern: "^/" },
      branchPrefix: {
        type: "string",
        pattern: "^[A-Za-z0-9._/-]+$",
        minLength: 1
      },
      maxWaveSize: { type: "integer", minimum: 1, maximum: 6 },
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
      }
    },
    additionalProperties: false
  },
  maxNodes: 14,
  selectors: []
};

const worklistLabel = "tally:worklist";

const acceptanceCriterionSchema = {
  type: "object",
  required: ["text", "checked"],
  properties: {
    text: { type: "string", minLength: 1 },
    checked: { type: "boolean" }
  },
  additionalProperties: false
};

const worklistEntrySchema = {
  type: "object",
  required: [
    "taskId",
    "title",
    "acceptanceCriteria",
    "parallelism"
  ],
  properties: {
    taskId: { type: "string", pattern: "^[1-9][0-9]*$" },
    title: { type: "string", minLength: 1 },
    acceptanceCriteria: {
      type: "array",
      minItems: 1,
      items: acceptanceCriterionSchema
    },
    parallelism: { enum: ["parallel", "sequential"] },
    files: {
      type: "array",
      uniqueItems: true,
      items: { type: "string", minLength: 1 }
    },
    dependsOn: {
      type: "array",
      uniqueItems: true,
      items: { type: "string", pattern: "^[1-9][0-9]*$" }
    }
  },
  additionalProperties: false
};

const workspaceSchema = {
  type: "object",
  required: ["taskId", "branch", "worktreePath"],
  properties: {
    taskId: { type: "string", pattern: "^[1-9][0-9]*$" },
    branch: { type: "string", minLength: 1 },
    worktreePath: { type: "string", pattern: "^/" }
  },
  additionalProperties: false
};

const worklistSchema = {
  type: "object",
  required: [
    "schemaVersion",
    "source",
    "baseRev",
    "entries",
    "wave",
    "workspaces"
  ],
  properties: {
    schemaVersion: { const: 1 },
    source: {
      type: "object",
      required: ["kind", "repository", "label"],
      properties: {
        kind: { const: "github-issues" },
        repository: { type: "string", minLength: 1 },
        label: { const: "tally:worklist" }
      },
      additionalProperties: false
    },
    baseRev: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    entries: {
      type: "array",
      uniqueItems: true,
      items: worklistEntrySchema
    },
    wave: {
      type: "array",
      maxItems: 6,
      uniqueItems: true,
      items: { type: "string", pattern: "^[1-9][0-9]*$" }
    },
    workspaces: {
      type: "array",
      maxItems: 6,
      uniqueItems: true,
      items: workspaceSchema
    }
  },
  additionalProperties: false
};

const driverErrorSchema = {
  type: "object",
  required: ["code", "message"],
  properties: {
    code: { type: "string", minLength: 1 },
    message: { type: "string", minLength: 1 },
    details: { type: "object" }
  },
  additionalProperties: false
};

function driverEnvelopeSchema(valueSchema) {
  return {
    oneOf: [
      {
        type: "object",
        required: ["ok", "value"],
        properties: {
          ok: { const: true },
          value: valueSchema
        },
        additionalProperties: false
      },
      {
        type: "object",
        required: ["ok", "error"],
        properties: {
          ok: { const: false },
          error: driverErrorSchema
        },
        additionalProperties: false
      }
    ]
  };
}

const implementationResultSchema = {
  type: "object",
  required: ["taskId", "branch", "head", "summary", "tests"],
  properties: {
    taskId: { type: "string", pattern: "^[1-9][0-9]*$" },
    branch: { type: "string", minLength: 1 },
    head: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    summary: { type: "string", minLength: 1, maxLength: 12000 },
    tests: {
      type: "array",
      items: { type: "string", minLength: 1, maxLength: 2000 }
    }
  },
  additionalProperties: false
};

const reviewResultSchema = {
  type: "object",
  required: ["taskId", "reviewedHead", "verdict", "summary", "findings"],
  properties: {
    taskId: { type: "string", pattern: "^[1-9][0-9]*$" },
    reviewedHead: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
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
  required: ["status", "reportPath", "pullRequests"],
  properties: {
    status: { enum: ["empty", "ready"] },
    reportPath: { type: "string", pattern: "^/" },
    pullRequests: {
      type: "array",
      maxItems: 6,
      items: {
        type: "object",
        required: ["taskId", "branch", "status", "url"],
        properties: {
          taskId: { type: "string", pattern: "^[1-9][0-9]*$" },
          branch: { type: "string", minLength: 1 },
          status: { enum: ["created", "existing", "no-changes"] },
          url: { type: ["string", "null"] }
        },
        additionalProperties: false
      }
    }
  },
  additionalProperties: false
};

function throwDriverError(errorValue) {
  const error = new Error(errorValue.message);
  error.name = "AgencyDriverError";
  error.code = errorValue.code;
  error.details = errorValue.details || {};
  throw error;
}

function unwrapDriver(node) {
  if (!node.result.ok) {
    throwDriverError(node.result.error);
  }
  return node.result.value;
}

function driverNode(action, brief, resultSchema, evidence) {
  return job({
    argv: [args.driver.program, action],
    adapter: args.driver.adapter,
    pools: ["agency-control"],
    priority: "low",
    runtimeMaxSec: args.driver.runtimeMaxSec,
    evidence: evidence || ["exit:0"],
    brief,
    key: action,
    label: `agency-${action}`,
    resultSchema: driverEnvelopeSchema(resultSchema)
  });
}

function entryById(worklist, taskId) {
  return worklist.entries.find(entry => entry.taskId === taskId);
}

function workspaceById(worklist, taskId) {
  return worklist.workspaces.find(workspace => workspace.taskId === taskId);
}

function validateWave(worklist) {
  if (worklist.source.repository !== args.repository) {
    const error = new Error("worklist repository does not match the configured repository");
    error.name = "AgencyWorklistError";
    error.code = "worklist-repository-mismatch";
    throw error;
  }
  if (worklist.wave.length > args.maxWaveSize) {
    const error = new Error("worklist driver returned a wave larger than maxWaveSize");
    error.name = "AgencyWorklistError";
    error.code = "worklist-wave-too-large";
    throw error;
  }
  for (const taskId of worklist.wave) {
    if (!entryById(worklist, taskId) || !workspaceById(worklist, taskId)) {
      const error = new Error(`worklist wave task ${taskId} has no entry or workspace`);
      error.name = "AgencyWorklistError";
      error.code = "worklist-contract-invalid";
      error.details = { taskId };
      throw error;
    }
  }
}

function implementationPrompt(task, workspace, baseRev) {
  return [
    `Implement GitHub worklist task #${task.taskId}: ${task.title}`,
    `Repository: ${args.repository}`,
    `Pinned base revision: ${baseRev}`,
    `Branch: ${workspace.branch}`,
    `Worktree: ${workspace.worktreePath}`,
    `Acceptance criteria: ${JSON.stringify(task.acceptanceCriteria)}`,
    `File hints: ${JSON.stringify(task.files || [])}`,
    `Dependencies: ${JSON.stringify(task.dependsOn || [])}`,
    "Work only in the assigned worktree. Implement the complete task, run proportionate tests, and commit the result to the assigned branch.",
    "Do not create or merge a pull request; the deterministic culmination owns that.",
    `Return only JSON matching {"taskId":"${task.taskId}","branch":"${workspace.branch}","head":"<40-or-64-hex-commit>","summary":"<bounded summary>","tests":["<command and outcome>"]}.`
  ].join("\n");
}

function reviewPrompt(task, workspace, implementation) {
  return [
    `Independently review GitHub worklist task #${task.taskId}: ${task.title}`,
    `Repository: ${args.repository}`,
    `Branch: ${workspace.branch}`,
    `Worktree: ${workspace.worktreePath}`,
    `Acceptance criteria: ${JSON.stringify(task.acceptanceCriteria)}`,
    `Implementation report: ${JSON.stringify(implementation.result)}`,
    "Review the committed diff and run any checks needed to assess the acceptance criteria.",
    "Do not modify the worktree, branch, issue, or pull request. The implementing harness never certifies its own work; this is the cross-harness verdict for the human culmination.",
    `Return only JSON matching {"taskId":"${task.taskId}","reviewedHead":"${implementation.result.head}","verdict":"approve|changes-requested","summary":"<bounded review>","findings":[{"severity":"blocking|non-blocking","text":"<finding>"}]}.`
  ].join("\n");
}

(async () => {
  const worklist = unwrapDriver(
    await driverNode(
      "worklist",
      {
        action: "worklist",
        source: {
          kind: "github-issues",
          repository: args.repository,
          label: worklistLabel,
          state: "open"
        },
        checkout: args.checkout,
        baseRev: args.baseRev,
        worktreeRoot: args.worktreeRoot,
        branchPrefix: args.branchPrefix,
        maxWaveSize: args.maxWaveSize
      },
      worklistSchema
    )
  );
  validateWave(worklist);

  const tasks = worklist.wave.map(taskId => ({
    entry: entryById(worklist, taskId),
    workspace: workspaceById(worklist, taskId)
  }));

  const implementations = await parallel(
    tasks.map(task => () =>
      codex(
        implementationPrompt(task.entry, task.workspace, worklist.baseRev),
        {
          priority: "low",
          workspace: {
            repo: args.repository,
            baseRev: worklist.baseRev,
            branch: task.workspace.branch,
            worktreePath: task.workspace.worktreePath
          },
          key: `implementation-${task.entry.taskId}`,
          label: `implement-${task.entry.taskId}`,
          resultSchema: implementationResultSchema
        }
      )
    )
  );

  const reviews = await parallel(
    tasks.map((task, index) => () =>
      claude(reviewPrompt(task.entry, task.workspace, implementations[index]), {
        priority: "low",
        workspace: {
          repo: args.repository,
          baseRev: worklist.baseRev,
          branch: task.workspace.branch,
          worktreePath: task.workspace.worktreePath
        },
        key: `review-${task.entry.taskId}`,
        label: `review-${task.entry.taskId}`,
        resultSchema: reviewResultSchema
      })
    )
  );

  const culminationTasks = tasks.map((task, index) => ({
    entry: task.entry,
    workspace: task.workspace,
    implementation: implementations[index].result,
    review: reviews[index].result
  }));
  const culmination = unwrapDriver(
    await driverNode(
      "culminate",
      {
        action: "culminate",
        source: worklist.source,
        repository: args.repository,
        checkout: args.checkout,
        baseRev: worklist.baseRev,
        baseBranch: args.baseBranch,
        reportPath: args.reportPath,
        tasks: culminationTasks
      },
      culminationSchema,
      ["exit:0", `artifact:${args.reportPath}`, "hash:sha256"]
    )
  );

  return {
    source: worklist.source,
    wave: worklist.wave,
    culmination
  };
})();
