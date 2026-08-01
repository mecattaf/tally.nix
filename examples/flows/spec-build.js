// Generic, stateless spec-repository build reconciler.
//
// Every invocation witnesses current forge state, selects one dependency-ready
// and conflict-disjoint frontier, advances those tasks in isolated worktrees,
// and exits. Merged pull requests are the durable checkboxes; a new mention or
// merge-triggered invocation always starts from those facts instead of a prior
// runner's witnessed prefix.
export const meta = {
  name: "spec-build",
  description: "Reconcile one witnessed spec-build frontier against merged pull requests",
  pools: ["campaign-agent", "campaign-control"],
  argsSchema: {
    type: "object",
    required: [
      "campaign",
      "repository",
      "issue",
      "runId",
      "repositories",
      "worklist",
      "maxTasks",
      "maxParallel",
      "reconcileCommand",
      "workspaceRoot",
      "driver",
      "driverRuntimeMaxSec",
      "agent",
      "gates"
    ],
    properties: {
      campaign: {
        type: "string",
        maxLength: 80,
        pattern: "^[A-Za-z0-9_][A-Za-z0-9_.-]*$"
      },
      repository: { type: "string", pattern: "^[^/ \\t]+/[^/ \\t]+$" },
      issue: {
        type: "object",
        required: ["number", "url"],
        properties: {
          number: { type: "string", pattern: "^[1-9][0-9]*$" },
          url: { type: "string", minLength: 1 }
        },
        additionalProperties: false
      },
      runId: { type: "string", minLength: 1, maxLength: 512 },
      repositories: {
        type: "object",
        minProperties: 1,
        additionalProperties: {
          type: "object",
          required: ["checkout", "baseBranch", "remote", "forge"],
          properties: {
            checkout: { type: "string", pattern: "^/" },
            baseBranch: { type: "string", pattern: "^[A-Za-z0-9._/+-]+$" },
            remote: { type: "string", pattern: "^[A-Za-z0-9._-]+$" },
            forge: { enum: ["github", "local"] }
          },
          additionalProperties: false
        }
      },
      worklist: { type: "string", minLength: 1 },
      maxTasks: { type: "integer", minimum: 1, maximum: 128 },
      maxParallel: { type: "integer", minimum: 1, maximum: 128 },
      reconcileCommand: { type: "string", pattern: "^/[^\\r\\n]+$", maxLength: 300 },
      workspaceRoot: { type: "string", pattern: "^/" },
      driver: { type: "string", pattern: "^/" },
      driverRuntimeMaxSec: { type: "integer", minimum: 1 },
      agent: {
        type: "object",
        required: [
          "adapter",
          "argv",
          "priority",
          "runtimeMaxSec",
          "approvalPolicy",
          "sandboxPolicy"
        ],
        properties: {
          adapter: { type: "string", minLength: 1 },
          argv: {
            type: "array",
            minItems: 1,
            items: { type: "string" }
          },
          priority: { enum: ["interrupt", "high", "medium", "low"] },
          runtimeMaxSec: { type: ["integer", "null"], minimum: 1 },
          approvalPolicy: { type: ["string", "null"], minLength: 1 },
          sandboxPolicy: { type: ["string", "null"], minLength: 1 }
        },
        additionalProperties: false
      },
      gates: {
        type: "array",
        minItems: 1,
        maxItems: 16,
        uniqueItems: true,
        items: {
          oneOf: [
            {
              type: "object",
              required: ["kind", "id", "preflightArgv", "argv", "runtimeMaxSec"],
              properties: {
                kind: { const: "command" },
                id: { type: "string", pattern: "^[A-Za-z0-9_][A-Za-z0-9_.-]*$" },
                preflightArgv: {
                  type: "array",
                  minItems: 1,
                  items: { type: "string" }
                },
                argv: {
                  type: "array",
                  minItems: 1,
                  items: { type: "string" }
                },
                runtimeMaxSec: { type: "integer", minimum: 1 }
              },
              additionalProperties: false
            },
            {
              type: "object",
              required: ["kind", "id", "forbidPaths", "runtimeMaxSec"],
              properties: {
                kind: { const: "forbidPaths" },
                id: { type: "string", pattern: "^[A-Za-z0-9_][A-Za-z0-9_.-]*$" },
                forbidPaths: {
                  type: "array",
                  minItems: 1,
                  maxItems: 128,
                  uniqueItems: true,
                  items: { type: "string", minLength: 1, maxLength: 1024 }
                },
                runtimeMaxSec: { type: "integer", minimum: 1 }
              },
              additionalProperties: false
            }
          ]
        }
      }
    },
    additionalProperties: false
  },
  // One pass is bounded by maxParallel <= 128 and gates <= 16. Before the
  // first merge, a separate pristine-base lane preflights every command gate
  // before the dependency-ready implementation frontier is admitted.
  iterationCap: 4096,
  selectors: []
};

const stringList = {
  type: "array",
  items: { type: "string", minLength: 1 }
};

const taskIdSchema = {
  type: "string",
  pattern: "^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$",
  maxLength: 80
};

const taskSchema = {
  type: "object",
  required: [
    "id",
    "title",
    "goal",
    "deliveredBehaviors",
    "readFirst",
    "acceptanceCriteria",
    "dependencies",
    "conflictDomains"
  ],
  properties: {
    id: taskIdSchema,
    title: { type: "string", minLength: 1, maxLength: 300 },
    goal: { type: "string", minLength: 1, maxLength: 12000 },
    deliveredBehaviors: {
      type: "array",
      minItems: 1,
      items: { type: "string", minLength: 1, maxLength: 4000 }
    },
    readFirst: {
      type: "object",
      required: ["specSections", "styleReferences"],
      properties: {
        specSections: {
          type: "array",
          minItems: 1,
          items: { type: "string", minLength: 1, maxLength: 1000 }
        },
        styleReferences: stringList
      },
      additionalProperties: false
    },
    acceptanceCriteria: {
      type: "array",
      minItems: 1,
      items: {
        type: "object",
        required: ["id", "description", "argv"],
        properties: {
          id: { type: "string", pattern: "^[A-Za-z0-9_][A-Za-z0-9_.-]*$" },
          description: { type: "string", minLength: 1, maxLength: 4000 },
          argv: {
            type: "array",
            minItems: 1,
            items: { type: "string" }
          }
        },
        additionalProperties: false
      }
    },
    dependencies: {
      type: "array",
      items: taskIdSchema
    },
    conflictDomains: stringList
  },
  additionalProperties: false
};

const sourceSchema = {
  type: "object",
  required: ["path", "sha256"],
  properties: {
    path: { type: "string", minLength: 1 },
    sha256: { type: "string", pattern: "^sha256:[0-9a-f]{64}$" }
  },
  additionalProperties: false
};

const mergedFactSchema = {
  type: "object",
  required: ["taskId", "pullRequest", "mergeCommit"],
  properties: {
    taskId: taskIdSchema,
    pullRequest: { type: "string", minLength: 1 },
    mergeCommit: { type: "string", pattern: "^[0-9a-f]{40,64}$" }
  },
  additionalProperties: false
};

const reconcileSchema = {
  type: "object",
  required: [
    "schemaVersion",
    "repository",
    "source",
    "tasks",
    "merged",
    "remaining",
    "frontier",
    "complete"
  ],
  properties: {
    schemaVersion: { const: 1 },
    repository: { type: "string", minLength: 1 },
    source: sourceSchema,
    tasks: {
      type: "array",
      minItems: 1,
      maxItems: 128,
      items: taskSchema
    },
    merged: { type: "array", items: mergedFactSchema },
    remaining: { type: "array", items: taskIdSchema },
    frontier: { type: "array", maxItems: 128, items: taskSchema },
    complete: { type: "boolean" }
  },
  additionalProperties: false
};

const workspaceSchema = {
  type: "object",
  required: ["taskId", "baseRev", "branch", "publishBranch", "worktreePath"],
  properties: {
    taskId: taskIdSchema,
    baseRev: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    branch: { type: "string", minLength: 1 },
    publishBranch: { type: "string", minLength: 1 },
    worktreePath: { type: "string", pattern: "^/" }
  },
  additionalProperties: false
};

const constraintSchema = {
  type: "object",
  required: ["gateId", "kind", "patterns", "checkedPaths", "baseRev", "head"],
  properties: {
    gateId: { type: "string", pattern: "^[A-Za-z0-9_][A-Za-z0-9_.-]*$" },
    kind: { const: "forbidPaths" },
    patterns: {
      type: "array",
      minItems: 1,
      maxItems: 128,
      uniqueItems: true,
      items: { type: "string", minLength: 1, maxLength: 1024 }
    },
    checkedPaths: { type: "integer", minimum: 0 },
    baseRev: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    head: { type: "string", pattern: "^[0-9a-f]{40,64}$" }
  },
  additionalProperties: false
};

const cleanupSchema = {
  type: "object",
  required: ["taskId", "cleaned"],
  properties: {
    taskId: taskIdSchema,
    cleaned: { const: true }
  },
  additionalProperties: false
};

const publicationSchema = {
  type: "object",
  required: ["taskId", "branch", "head", "pullRequest"],
  properties: {
    taskId: taskIdSchema,
    branch: { type: "string", minLength: 1 },
    head: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    pullRequest: { type: "string", minLength: 1 }
  },
  additionalProperties: false
};

const integrationSchema = {
  type: "object",
  required: ["taskId", "baseRev", "branch", "head", "pullRequest", "regate"],
  properties: {
    taskId: taskIdSchema,
    baseRev: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    branch: { type: "string", minLength: 1 },
    head: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    pullRequest: { type: "string", minLength: 1 },
    regate: { type: "boolean" }
  },
  additionalProperties: false
};

const mergeSchema = {
  type: "object",
  required: ["taskId", "head", "mergeCommit", "pullRequest", "regated"],
  properties: {
    taskId: taskIdSchema,
    head: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    mergeCommit: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    pullRequest: { type: "string", minLength: 1 },
    regated: { type: "boolean" }
  },
  additionalProperties: false
};

function driverNode(
  action,
  brief,
  key,
  label,
  resultSchema,
  workspace,
  settle,
  taskRef,
  runtimeMaxSec = args.driverRuntimeMaxSec
) {
  const spec = {
    argv: [args.driver, action],
    adapter: "spec-build-driver",
    pools: ["campaign-control"],
    priority: "low",
    runtimeMaxSec,
    evidence: ["exit:0"],
    brief,
    key,
    label
  };
  if (resultSchema !== null) {
    spec.resultSchema = resultSchema;
  }
  if (taskRef !== null) {
    spec.taskRef = taskRef;
  }
  if (workspace !== null) {
    spec.workspace = workspace;
  }
  return job(spec, { settle });
}

function workspaceFor(prepared, baseRev) {
  return {
    repo: args.repository,
    baseRev: baseRev || prepared.baseRev,
    branch: prepared.branch,
    worktreePath: prepared.worktreePath
  };
}

function nodePassed(node) {
  return node && node.verdict === "pass" && node.result !== null;
}

function bounded(value, limit) {
  const text = typeof value === "string" ? value : JSON.stringify(value);
  if (typeof text !== "string" || text.length === 0) {
    return "no detail";
  }
  return text.length > limit ? `${text.slice(0, limit)}...` : text;
}

function nodeFailure(task, stage, node) {
  return {
    taskId: task.id,
    stage,
    verdict: node && node.verdict ? node.verdict : "flow-error",
    detail: bounded(node && node.error ? node.error : node, 2000)
  };
}

async function runGate(task, gate, workspace, prefix) {
  const key = `${prefix}-${task.id}-${gate.id}`;
  const taskRef = `${args.campaign}/${task.id}`;
  if (gate.kind === "command") {
    return sh(gate.argv, {
      pools: ["campaign-control"],
      priority: "low",
      workspace,
      env: { CAMPAIGN_TASK_ID: task.id },
      runtimeMaxSec: gate.runtimeMaxSec,
      evidence: ["exit:0"],
      key,
      label: key,
      settle: true,
      taskRef
    });
  }
  return driverNode(
    "constraint",
    {
      gate,
      workspace: {
        taskId: task.id,
        baseRev: workspace.baseRev,
        branch: workspace.branch,
        worktreePath: workspace.worktreePath
      }
    },
    key,
    key,
    constraintSchema,
    workspace,
    true,
    taskRef,
    gate.runtimeMaxSec
  );
}

async function runPreflightGate(task, gate, workspace) {
  return sh(gate.preflightArgv, {
    pools: ["campaign-control"],
    priority: "low",
    workspace,
    env: { CAMPAIGN_TASK_ID: task.id },
    runtimeMaxSec: gate.runtimeMaxSec,
    evidence: ["exit:0"],
    key: `preflight-gate-${gate.id}`,
    label: `preflight-gate-${gate.id}`,
    settle: true,
    taskRef: `${args.campaign}/${task.id}`
  });
}

(async () => {
  const gateIds = [];
  for (const gate of args.gates) {
    if (gateIds.indexOf(gate.id) !== -1) {
      const error = new Error(`campaign gate id ${gate.id} is duplicated`);
      error.name = "SpecBuildConfigurationError";
      error.code = "duplicate-gate-id";
      throw error;
    }
    gateIds.push(gate.id);
  }

  const repositoryConfig = args.repositories[args.repository];
  if (!repositoryConfig) {
    const error = new Error(`campaign repository ${args.repository} is not configured`);
    error.name = "SpecBuildConfigurationError";
    error.code = "repository-not-configured";
    throw error;
  }

  const reconciliationNode = await driverNode(
    "reconcile",
    {
      campaign: args.campaign,
      repository: args.repository,
      repositoryConfig,
      issue: args.issue,
      worklist: args.worklist,
      maxTasks: args.maxTasks,
      maxParallel: args.maxParallel
    },
    "reconcile",
    "spec-build-reconcile",
    reconcileSchema,
    null,
    false,
    null
  );
  const reconciliation = reconciliationNode.result;

  // A merged campaign PR is the durable proof that an earlier pass cleared
  // admission. Until that first merge exists, every fresh pass gates a
  // separate pristine-base lane and cleans it before any agent can start.
  const commandGates = args.gates.filter(gate => gate.kind === "command");
  if (
    !reconciliation.complete &&
    reconciliation.merged.length === 0 &&
    commandGates.length > 0
  ) {
    const preflightTask = reconciliation.frontier[0];
    const preflightTaskRef = `${args.campaign}/${preflightTask.id}`;
    const preflight = await driverNode(
      "preflight",
      {
        campaign: args.campaign,
        repository: args.repository,
        repositoryConfig,
        issue: args.issue,
        runId: args.runId,
        workspaceRoot: args.workspaceRoot
      },
      "preflight-prep",
      "preflight-prep",
      workspaceSchema,
      null,
      false,
      preflightTaskRef
    );
    const preflightWorkspace = workspaceFor(preflight.result);
    let failedGate = null;
    for (const gate of commandGates) {
      const gated = await runPreflightGate(preflightTask, gate, preflightWorkspace);
      if (gated.verdict !== "pass") {
        failedGate = { gate, node: gated };
        break;
      }
    }
    await driverNode(
      "cleanup",
      { repositoryConfig, workspace: preflight.result },
      "preflight-cleanup",
      "preflight-cleanup",
      cleanupSchema,
      null,
      false,
      preflightTaskRef
    );
    if (failedGate !== null) {
      const error = new Error(
        `campaign preflight gate ${failedGate.gate.id} failed: ${bounded(failedGate.node, 2000)}`
      );
      error.name = "SpecBuildPreflightError";
      error.code = "preflight-failed";
      error.details = {
        gateId: failedGate.gate.id,
        node: failedGate.node
      };
      throw error;
    }
  }

  const laneOutcomes = await parallel(
    reconciliation.frontier.map(task => () => (async () => {
      const taskRef = `${args.campaign}/${task.id}`;
      const prepared = await driverNode(
        "prep",
        {
          campaign: args.campaign,
          repository: args.repository,
          repositoryConfig,
          issue: args.issue,
          runId: args.runId,
          workspaceRoot: args.workspaceRoot,
          task
        },
        `prep-${task.id}`,
        `prep-${task.id}`,
        workspaceSchema,
        null,
        true,
        taskRef
      );
      if (!nodePassed(prepared)) {
        return { task, failure: nodeFailure(task, "prep", prepared) };
      }
      const workspace = workspaceFor(prepared.result);

      const agentSpec = {
        argv: args.agent.argv,
        adapter: args.agent.adapter,
        pools: ["campaign-agent"],
        priority: args.agent.priority,
        workspace,
        evidence: ["exit:0"],
        brief: {
          schemaVersion: 1,
          mission: `Implement only spec-build task ${task.id}: ${task.title}. Commit the complete result on the assigned branch. Do not push, open a pull request, merge, or read another task from the worklist; deterministic campaign nodes own those operations. Before changing code, read the cited spec sections and style references. Read the campaign issue comments for steering at the start of this attempt. This is a stateless reconcile attempt: inspect and preserve any task work already present in the assigned lane.`,
          campaign: {
            name: args.campaign,
            repository: args.repository,
            issue: args.issue,
            runId: args.runId
          },
          task,
          workspace: prepared.result,
          steering: {
            channel: "github-issue-comments",
            repository: args.repository,
            issueNumber: args.issue.number,
            issueUrl: args.issue.url
          }
        },
        key: `agent-${task.id}`,
        label: `agent-${task.id}`,
        taskRef
      };
      if (args.agent.runtimeMaxSec !== null) {
        agentSpec.runtimeMaxSec = args.agent.runtimeMaxSec;
      }
      if (args.agent.approvalPolicy !== null) {
        agentSpec.approvalPolicy = args.agent.approvalPolicy;
      }
      if (args.agent.sandboxPolicy !== null) {
        agentSpec.sandboxPolicy = args.agent.sandboxPolicy;
      }
      const agent = await job(agentSpec, { settle: true });
      if (agent.verdict !== "pass") {
        return { task, prepared: prepared.result, failure: nodeFailure(task, "agent", agent) };
      }

      const constraintResults = [];
      for (const gate of args.gates) {
        const gated = await runGate(task, gate, workspace, "gate");
        if (gated.verdict !== "pass") {
          return {
            task,
            prepared: prepared.result,
            failure: nodeFailure(task, `gate:${gate.id}`, gated)
          };
        }
        if (gate.kind === "forbidPaths") {
          constraintResults.push(gated.result);
        }
      }

      const publication = await driverNode(
        "publish",
        {
          campaign: args.campaign,
          repository: args.repository,
          repositoryConfig,
          issue: args.issue,
          runId: args.runId,
          workspaceRoot: args.workspaceRoot,
          task,
          workspace: prepared.result,
          constraints: constraintResults
        },
        `publish-${task.id}`,
        `publish-${task.id}`,
        publicationSchema,
        workspace,
        true,
        taskRef
      );
      if (!nodePassed(publication)) {
        return {
          task,
          prepared: prepared.result,
          failure: nodeFailure(task, "publish", publication)
        };
      }
      return {
        task,
        prepared: prepared.result,
        publication: publication.result,
        constraints: constraintResults
      };
    })()),
    { settle: true }
  );

  const lanes = laneOutcomes.map((outcome, index) => {
    if (outcome.ok) {
      return outcome.value;
    }
    const task = reconciliation.frontier[index];
    return { task, failure: nodeFailure(task, "lane", outcome.error) };
  });
  const failures = lanes.filter(lane => lane.failure).map(lane => lane.failure);
  const publications = lanes.filter(lane => lane.publication);
  const merged = [];

  // Publication work is parallel; integration is deliberately ordered. Before
  // every merge the driver compares the tested base to current main. Only a
  // moved base causes a rebase and a second witnessed gate pass.
  for (const lane of publications) {
    const task = lane.task;
    const taskRef = `${args.campaign}/${task.id}`;
    const workspace = workspaceFor(lane.prepared);
    const integration = await driverNode(
      "rebase",
      {
        campaign: args.campaign,
        repository: args.repository,
        repositoryConfig,
        issue: args.issue,
        runId: args.runId,
        workspaceRoot: args.workspaceRoot,
        task,
        workspace: lane.prepared,
        publication: lane.publication,
        constraints: lane.constraints
      },
      `rebase-${task.id}`,
      `rebase-${task.id}`,
      integrationSchema,
      workspace,
      true,
      taskRef
    );
    if (!nodePassed(integration)) {
      failures.push(nodeFailure(task, "rebase", integration));
      continue;
    }

    let regateFailed = false;
    if (integration.result.regate) {
      const integratedWorkspace = workspaceFor(lane.prepared, integration.result.baseRev);
      for (const gate of args.gates) {
        const gated = await runGate(task, gate, integratedWorkspace, "regate");
        if (gated.verdict !== "pass") {
          failures.push(nodeFailure(task, `regate:${gate.id}`, gated));
          regateFailed = true;
          break;
        }
      }
    }
    if (regateFailed) {
      continue;
    }

    const merge = await driverNode(
      "merge",
      {
        campaign: args.campaign,
        repository: args.repository,
        repositoryConfig,
        issue: args.issue,
        runId: args.runId,
        reconcileCommand: args.reconcileCommand,
        workspaceRoot: args.workspaceRoot,
        task,
        workspace: lane.prepared,
        integration: integration.result
      },
      `merge-${task.id}`,
      `merge-${task.id}`,
      mergeSchema,
      null,
      true,
      taskRef
    );
    if (!nodePassed(merge)) {
      failures.push(nodeFailure(task, "merge", merge));
      continue;
    }
    merged.push(merge.result);
  }

  return {
    campaign: args.campaign,
    repository: args.repository,
    issue: args.issue,
    worklist: reconciliation.source,
    state: reconciliation.complete
      ? "complete"
      : merged.length > 0
        ? "advanced"
        : failures.length > 0
          ? "needs-attention"
          : "idle",
    reconciled: {
      merged: reconciliation.merged,
      remaining: reconciliation.remaining,
      frontier: reconciliation.frontier.map(task => task.id)
    },
    merged,
    failures
  };
})();
