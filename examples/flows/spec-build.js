// Generic, serial spec-repository build campaign.
//
// The work graph is data in the repository's tasks artifact. This flow only
// interprets that witnessed worklist: for every task it prepares from current
// main, delivers one structured brief, runs configured direct-argv gates,
// publishes a pull request, and merges it before preparing the next task.
export const meta = {
  name: "spec-build",
  description: "Build a frozen spec corpus one witnessed, merged task at a time",
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
      "workspaceRoot",
      "driver",
      "driverRuntimeMaxSec",
      "agent",
      "gates"
    ],
    properties: {
      campaign: { type: "string", pattern: "^[A-Za-z0-9_][A-Za-z0-9_.-]*$" },
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
        items: {
          type: "object",
          required: ["id", "argv"],
          properties: {
            id: { type: "string", pattern: "^[A-Za-z0-9_][A-Za-z0-9_.-]*$" },
            argv: {
              type: "array",
              minItems: 1,
              items: { type: "string" }
            }
          },
          additionalProperties: false
        }
      }
    },
    additionalProperties: false
  },
  // A campaign is bounded by maxTasks <= 128 and gates <= 16. Its node budget
  // includes one worklist node, one base preflight per gate, and every
  // per-task prep/agent/gates/publish/merge chain.
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
    "dependencies"
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
    }
  },
  additionalProperties: false
};

const worklistSchema = {
  type: "object",
  required: ["schemaVersion", "repository", "source", "tasks"],
  properties: {
    schemaVersion: { const: 1 },
    repository: { type: "string", minLength: 1 },
    source: {
      type: "object",
      required: ["path", "sha256"],
      properties: {
        path: { type: "string", minLength: 1 },
        sha256: { type: "string", pattern: "^sha256:[0-9a-f]{64}$" }
      },
      additionalProperties: false
    },
    tasks: {
      type: "array",
      minItems: 1,
      maxItems: 128,
      items: taskSchema
    }
  },
  additionalProperties: false
};

const workspaceSchema = {
  type: "object",
  required: ["taskId", "baseRev", "branch", "worktreePath"],
  properties: {
    taskId: taskIdSchema,
    baseRev: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    branch: { type: "string", minLength: 1 },
    worktreePath: { type: "string", pattern: "^/" }
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

const mergeSchema = {
  type: "object",
  required: ["taskId", "head", "mergeCommit", "pullRequest"],
  properties: {
    taskId: taskIdSchema,
    head: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    mergeCommit: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    pullRequest: { type: "string", minLength: 1 }
  },
  additionalProperties: false
};

function driverNode(action, brief, key, label, resultSchema, workspace) {
  const spec = {
    argv: [args.driver, action],
    adapter: "spec-build-driver",
    pools: ["campaign-control"],
    priority: "low",
    runtimeMaxSec: args.driverRuntimeMaxSec,
    evidence: ["exit:0"],
    brief,
    key,
    label,
    resultSchema
  };
  if (workspace !== null) {
    spec.workspace = workspace;
  }
  return job(spec);
}

function workspaceFor(prepared) {
  return {
    repo: args.repository,
    baseRev: prepared.baseRev,
    branch: prepared.branch,
    worktreePath: prepared.worktreePath
  };
}

(async () => {
  const repositoryConfig = args.repositories[args.repository];
  if (!repositoryConfig) {
    const error = new Error(`campaign repository ${args.repository} is not configured`);
    error.name = "SpecBuildConfigurationError";
    error.code = "repository-not-configured";
    throw error;
  }

  const worklistNode = await driverNode(
    "worklist",
    {
      repository: args.repository,
      repositoryConfig,
      worklist: args.worklist,
      maxTasks: args.maxTasks
    },
    "worklist",
    "spec-build-worklist",
    worklistSchema,
    null
  );
  const worklist = worklistNode.result;
  const merged = [];

  // Serial by construction. The merge for task N is witnessed before prep for
  // task N+1 is even submitted, so the next base revision is current main.
  for (const task of worklist.tasks) {
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
      null
    );
    const workspace = workspaceFor(prepared.result);

    // The first prepared worktree is a fresh checkout of the fetched remote
    // base. Prove every exact gate argv there before an implementation adapter
    // is admitted; these named nodes become a clear, replayable receipt.
    if (merged.length === 0) {
      for (const gate of args.gates) {
        await sh(gate.argv, {
          pools: ["campaign-control"],
          priority: "low",
          workspace,
          evidence: ["exit:0"],
          key: `preflight-gate-${gate.id}`,
          label: `preflight-gate-${gate.id}`
        });
      }
    }

    const agentSpec = {
      argv: args.agent.argv,
      adapter: args.agent.adapter,
      pools: ["campaign-agent"],
      priority: args.agent.priority,
      workspace,
      evidence: ["exit:0"],
      brief: {
        schemaVersion: 1,
        mission: `Implement only spec-build task ${task.id}: ${task.title}. Commit the complete result on the assigned branch. Do not push, open a pull request, merge, or read another task from the worklist; deterministic campaign nodes own those operations. Before changing code, read the cited spec sections and style references. Read the campaign issue comments for steering at the start of this attempt.`,
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
      label: `agent-${task.id}`
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
    await job(agentSpec);

    for (const gate of args.gates) {
      await sh(gate.argv, {
        pools: ["campaign-control"],
        priority: "low",
        workspace,
        env: { CAMPAIGN_TASK_ID: task.id },
        evidence: ["exit:0"],
        key: `gate-${task.id}-${gate.id}`,
        label: `gate-${task.id}-${gate.id}`
      });
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
        workspace: prepared.result
      },
      `publish-${task.id}`,
      `publish-${task.id}`,
      publicationSchema,
      workspace
    );

    const merge = await driverNode(
      "merge",
      {
        campaign: args.campaign,
        repository: args.repository,
        repositoryConfig,
        issue: args.issue,
        runId: args.runId,
        workspaceRoot: args.workspaceRoot,
        task,
        workspace: prepared.result,
        publication: publication.result
      },
      `merge-${task.id}`,
      `merge-${task.id}`,
      mergeSchema,
      null
    );
    merged.push(merge.result);
  }

  return {
    campaign: args.campaign,
    repository: args.repository,
    issue: args.issue,
    worklist: worklist.source,
    merged
  };
})();
