// Generic, stateless spec-repository build reconciler.
//
// Every invocation witnesses current forge state, selects one dependency-ready
// and conflict-disjoint frontier, advances those tasks in isolated worktrees,
// and exits. Merged pull requests, content-bound checkpoint refs, machine
// diagnosis comments, and the one escalation marker are durable forge facts;
// every invocation starts from those facts instead of a prior runner's
// witnessed prefix.
export const meta = {
  name: "spec-build",
  description: "Reconcile one witnessed spec-build frontier against durable forge state",
  pools: ["campaign-agent", "campaign-control"],
  argsSchema: {
    type: "object",
    required: [
      "repository",
      "issue",
      "runId",
      "worklist",
      "workspaceRoot",
      "driver",
      "driverRuntimeMaxSec"
    ],
    properties: {
      campaign: {
        type: "string",
        maxLength: 80,
        pattern: "^[A-Za-z0-9_][A-Za-z0-9_.-]*$"
      },
      campaignIdentity: {
        type: "string",
        pattern: "^[0-9a-fA-F-]{36}$"
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
      worklist: {
        oneOf: [
          { type: "string", minLength: 1 },
          {
            type: "object",
            required: ["kind", "graphDigest"],
            properties: {
              kind: { const: "github-issue" },
              graphDigest: { type: "string", pattern: "^sha256:[0-9a-f]{64}$" }
            },
            additionalProperties: false
          }
        ]
      },
      maxTasks: { type: "integer", minimum: 1, maximum: 128 },
      maxParallel: { type: "integer", minimum: 1, maximum: 128 },
      reconcileCommand: { type: "string", pattern: "^/[^\\r\\n]+$", maxLength: 300 },
      workspaceRoot: { type: "string", pattern: "^/" },
      tally: { type: "string", pattern: "^/" },
      driver: { type: "string", pattern: "^/" },
      driverRuntimeMaxSec: { type: "integer", minimum: 1 },
      steering: {
        type: "array",
        maxItems: 1000,
        items: {
          type: "object",
          required: ["id", "url", "author", "body", "createdAt", "updatedAt"],
          properties: {
            id: { type: "integer", minimum: 1 },
            url: { type: "string", minLength: 1 },
            author: { type: "string", minLength: 1, maxLength: 39 },
            body: { type: "string", maxLength: 64000 },
            createdAt: { type: "string", minLength: 1 },
            updatedAt: { type: "string", minLength: 1 }
          },
          additionalProperties: false
        }
      },
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
            items: { type: "string", minLength: 1, pattern: "^[^\\u0000-\\u001f\\u007f]+$" }
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
                  items: { type: "string", minLength: 1, pattern: "^[^\\u0000-\\u001f\\u007f]+$" }
                },
                argv: {
                  type: "array",
                  minItems: 1,
                  items: { type: "string", minLength: 1, pattern: "^[^\\u0000-\\u001f\\u007f]+$" }
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
    oneOf: [
      {
        required: [
          "campaign",
          "repositories",
          "maxTasks",
          "maxParallel",
          "reconcileCommand",
          "agent",
          "gates"
        ]
      },
      {
        required: ["campaignIdentity", "steering", "tally"],
        properties: {
          worklist: {
            type: "object",
            required: ["kind", "graphDigest"],
            properties: {
              kind: { const: "github-issue" },
              graphDigest: { type: "string", pattern: "^sha256:[0-9a-f]{64}$" }
            },
            additionalProperties: false
          }
        }
      }
    ],
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

const implementationTaskSchema = {
  type: "object",
  required: [
    "id",
    "kind",
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
    kind: { const: "implementation" },
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

const checkpointTaskSchema = {
  type: "object",
  required: ["id", "kind", "title", "argv", "runtimeMaxSec", "dependencies"],
  properties: {
    id: taskIdSchema,
    kind: { const: "checkpoint" },
    title: { type: "string", minLength: 1, maxLength: 300 },
    argv: {
      type: "array",
      minItems: 1,
      items: { type: "string", minLength: 1, pattern: "^[^\\u0000-\\u001f\\u007f]+$" }
    },
    runtimeMaxSec: { type: "integer", minimum: 1 },
    dependencies: {
      type: "array",
      items: taskIdSchema
    }
  },
  additionalProperties: false
};

const issueTaskSchema = {
  type: "object",
  required: ["id", "kind", "title", "brief", "dependencies", "conflictDomains", "revision"],
  properties: {
    id: taskIdSchema,
    kind: { const: "implementation" },
    title: { type: "string", minLength: 1, maxLength: 300 },
    brief: {
      type: "object",
      required: ["issue", "body"],
      properties: {
        issue: {
          type: "object",
          required: ["number", "url"],
          properties: {
            number: { type: "string", pattern: "^[1-9][0-9]*$" },
            url: { type: "string", minLength: 1 }
          },
          additionalProperties: false
        },
        body: { type: "string", minLength: 1, maxLength: 64000 }
      },
      additionalProperties: false
    },
    dependencies: { type: "array", items: taskIdSchema },
    conflictDomains: stringList,
    revision: { type: "string", pattern: "^sha256:[0-9a-f]{64}$" }
  },
  additionalProperties: false
};

const issueCheckpointTaskSchema = {
  type: "object",
  required: [
    "id",
    "kind",
    "title",
    "brief",
    "argv",
    "runtimeMaxSec",
    "dependencies",
    "revision"
  ],
  properties: {
    id: taskIdSchema,
    kind: { const: "checkpoint" },
    title: { type: "string", minLength: 1, maxLength: 300 },
    brief: issueTaskSchema.properties.brief,
    argv: {
      type: "array",
      minItems: 1,
      items: { type: "string", minLength: 1, pattern: "^[^\\u0000-\\u001f\\u007f]+$" }
    },
    runtimeMaxSec: { type: "integer", minimum: 1 },
    dependencies: { type: "array", items: taskIdSchema },
    revision: { type: "string", pattern: "^sha256:[0-9a-f]{64}$" }
  },
  additionalProperties: false
};

const taskSchema = {
  oneOf: [implementationTaskSchema, checkpointTaskSchema, issueTaskSchema, issueCheckpointTaskSchema]
};

const sourceSchema = {
  oneOf: [
    {
      type: "object",
      required: ["path", "sha256", "revision"],
      properties: {
        path: { type: "string", minLength: 1 },
        sha256: { type: "string", pattern: "^sha256:[0-9a-f]{64}$" },
        revision: { type: "string", pattern: "^[0-9a-f]{40,64}$" }
      },
      additionalProperties: false
    },
    {
      type: "object",
      required: ["kind", "url", "sha256", "revision"],
      properties: {
        kind: { const: "github-issue" },
        url: { type: "string", minLength: 1 },
        sha256: { type: "string", pattern: "^sha256:[0-9a-f]{64}$" },
        revision: { type: "string", pattern: "^[0-9a-f]{40,64}$" }
      },
      additionalProperties: false
    }
  ]
};

const mergedFactSchema = {
  type: "object",
  required: ["taskId", "pullRequest", "mergeCommit"],
  properties: {
    taskId: taskIdSchema,
    pullRequest: { type: "string", minLength: 1 },
    mergeCommit: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    revision: { type: "string", pattern: "^sha256:[0-9a-f]{64}$" }
  },
  additionalProperties: false
};

const checkpointFactSchema = {
  type: "object",
  required: ["taskId", "ref", "revision"],
  properties: {
    taskId: taskIdSchema,
    ref: { type: "string", pattern: "^refs/tags/tally/spec-build/v1/" },
    revision: { type: "string", pattern: "^[0-9a-f]{40,64}$" }
  },
  additionalProperties: false
};

const effectiveConfigSchema = {
  type: "object",
  required: [
    "campaign",
    "repositoryConfig",
    "maxParallel",
    "agent",
    "gates",
    "reconcileCommand"
  ],
  properties: {
    campaign: {
      type: "string",
      maxLength: 80,
      pattern: "^[A-Za-z0-9_][A-Za-z0-9_.-]*$"
    },
    repositoryConfig: {
      type: "object",
      required: ["checkout", "baseBranch", "remote", "forge"],
      properties: {
        checkout: { type: "string", pattern: "^/" },
        baseBranch: { type: "string", minLength: 1 },
        remote: { type: "string", minLength: 1 },
        forge: { enum: ["github", "local"] }
      },
      additionalProperties: false
    },
    maxParallel: { type: "integer", minimum: 1, maximum: 128 },
    agent: { type: "object" },
    gates: { type: "array", minItems: 1, maxItems: 16 },
    reconcileCommand: { type: ["string", "null"] }
  },
  additionalProperties: false
};

const diagnosisFactSchema = {
  type: "object",
  required: ["taskId", "attempt", "comment", "diagnosis"],
  properties: {
    taskId: taskIdSchema,
    attempt: { type: "integer", minimum: 1, maximum: 2 },
    comment: { type: "string", minLength: 1 },
    diagnosis: { type: "string", minLength: 1, maxLength: 12000 }
  },
  additionalProperties: false
};

const blockedFactSchema = {
  type: "object",
  required: ["taskId", "blockedBy"],
  properties: {
    taskId: taskIdSchema,
    blockedBy: {
      type: "array",
      minItems: 1,
      uniqueItems: true,
      items: taskIdSchema
    }
  },
  additionalProperties: false
};

const reconcileSchema = {
  type: "object",
  required: [
    "schemaVersion",
    "campaign",
    "repository",
    "source",
    "tasks",
    "merged",
    "checkpoints",
    "remaining",
    "frontier",
    "diagnoses",
    "blocked",
    "quiescent",
    "escalation",
    "complete",
    "warnings"
  ],
  properties: {
    schemaVersion: { const: 1 },
    campaign: {
      type: "string",
      maxLength: 80,
      pattern: "^[A-Za-z0-9_][A-Za-z0-9_.-]*$"
    },
    repository: { type: "string", minLength: 1 },
    source: sourceSchema,
    tasks: {
      type: "array",
      minItems: 1,
      maxItems: 128,
      items: taskSchema
    },
    merged: { type: "array", items: mergedFactSchema },
    checkpoints: { type: "array", items: checkpointFactSchema },
    remaining: { type: "array", items: taskIdSchema },
    frontier: { type: "array", maxItems: 128, items: taskSchema },
    diagnoses: { type: "array", maxItems: 256, items: diagnosisFactSchema },
    blocked: { type: "array", maxItems: 128, items: blockedFactSchema },
    quiescent: { type: "boolean" },
    escalation: { type: ["string", "null"], minLength: 1 },
    complete: { type: "boolean" },
    warnings: stringList,
    config: effectiveConfigSchema
  },
  additionalProperties: false
};

const sweepSchema = {
  type: "object",
  required: ["currentRunHash", "cleaned", "warnings"],
  properties: {
    currentRunHash: { type: "string", pattern: "^[0-9a-f]{12}$" },
    cleaned: stringList,
    warnings: stringList
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

const ownershipSchema = {
  type: "object",
  required: [
    "taskId",
    "domainsRequired",
    "conflictDomains",
    "ownedPaths",
    "baseRev",
    "head"
  ],
  properties: {
    taskId: taskIdSchema,
    domainsRequired: { type: "boolean" },
    conflictDomains: {
      type: "array",
      uniqueItems: true,
      items: { type: "string", minLength: 1 }
    },
    ownedPaths: {
      type: "array",
      uniqueItems: true,
      items: { type: "string", minLength: 1 }
    },
    baseRev: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    head: { type: "string", pattern: "^[0-9a-f]{40,64}$" }
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

const checkpointCompletionSchema = {
  type: "object",
  required: ["taskId", "ref", "revision"],
  properties: {
    taskId: taskIdSchema,
    ref: { type: "string", pattern: "^refs/tags/tally/spec-build/v1/" },
    revision: { type: "string", pattern: "^[0-9a-f]{40,64}$" }
  },
  additionalProperties: false
};

const publicationSchema = {
  type: "object",
  required: ["taskId", "branch", "head", "pullRequest", "ownership"],
  properties: {
    taskId: taskIdSchema,
    branch: { type: "string", minLength: 1 },
    head: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    pullRequest: { type: "string", minLength: 1 },
    ownership: ownershipSchema
  },
  additionalProperties: false
};

const integrationSchema = {
  type: "object",
  required: [
    "taskId",
    "baseRev",
    "branch",
    "head",
    "pullRequest",
    "regate",
    "ownership"
  ],
  properties: {
    taskId: taskIdSchema,
    baseRev: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    branch: { type: "string", minLength: 1 },
    head: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    pullRequest: { type: "string", minLength: 1 },
    regate: { type: "boolean" },
    ownership: ownershipSchema
  },
  additionalProperties: false
};

const mergeSchema = {
  type: "object",
  required: ["taskId", "head", "mergeCommit", "pullRequest", "regated", "ownership"],
  properties: {
    taskId: taskIdSchema,
    head: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    mergeCommit: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    pullRequest: { type: "string", minLength: 1 },
    regated: { type: "boolean" },
    ownership: ownershipSchema
  },
  additionalProperties: false
};

const diffSchema = {
  type: "object",
  required: [
    "taskId",
    "available",
    "baseRev",
    "head",
    "status",
    "patch",
    "truncated",
    "reason"
  ],
  properties: {
    taskId: taskIdSchema,
    available: { type: "boolean" },
    baseRev: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    head: { type: ["string", "null"], pattern: "^[0-9a-f]{40,64}$" },
    status: { type: "string", maxLength: 16032 },
    patch: { type: "string", maxLength: 131104 },
    truncated: { type: "boolean" },
    reason: { type: ["string", "null"], minLength: 1 }
  },
  additionalProperties: false
};

const steeringSchema = {
  type: "object",
  required: ["taskId", "attempt", "comment", "blocked", "posted", "redacted"],
  properties: {
    taskId: taskIdSchema,
    attempt: { type: "integer", minimum: 1, maximum: 2 },
    comment: { type: "string", minLength: 1 },
    blocked: { type: "boolean" },
    posted: { type: "boolean" },
    redacted: { type: "boolean" }
  },
  additionalProperties: false
};

const escalationSchema = {
  type: "object",
  required: ["posted", "comment", "diagnosisCount"],
  properties: {
    posted: { type: "boolean" },
    comment: { type: "string", minLength: 1 },
    diagnosisCount: { type: "integer", minimum: 1, maximum: 256 }
  },
  additionalProperties: false
};

const continuationSchema = {
  type: "object",
  required: ["command", "posted"],
  properties: {
    command: { type: "string", pattern: "^/[^\\r\\n]+$", maxLength: 300 },
    posted: { const: true }
  },
  additionalProperties: false
};

const diagnosisResultSchema = {
  type: "string",
  minLength: 1,
  maxLength: 12000
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

let effective = null;
let campaignTaskIdentity = null;

function taskRefFor(taskId) {
  return `${campaignTaskIdentity}/${taskId}`;
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

function failureReport(task, stage, node) {
  return {
    taskId: task.id,
    stage,
    verdict: node && node.verdict ? node.verdict : "flow-error",
    detail: bounded(node && node.error ? node.error : node, 2000)
  };
}

function cleanupBrief(repositoryConfig, taskId, workspace) {
  const brief = {
    campaign: effective.campaign,
    repository: args.repository,
    repositoryConfig,
    runId: args.runId,
    workspaceRoot: args.workspaceRoot,
    taskId
  };
  if (workspace !== null) {
    brief.workspace = workspace;
  }
  return brief;
}

function taskFailure(task, stage, node, taskBrief, gateOutputs, prepared, baseRev) {
  return {
    task,
    report: failureReport(task, stage, node),
    stage,
    node,
    taskBrief,
    gateOutputs: gateOutputs || [],
    prepared: prepared || null,
    baseRev: baseRev || (prepared && prepared.baseRev) || null
  };
}

function machineDiagnoses(reconciliation, taskId) {
  return reconciliation.diagnoses.filter(item => item.taskId === taskId);
}

function implementationBrief(task, prepared, reconciliation) {
  return {
    schemaVersion: 1,
    mission: task.brief
      ? `Implement only forge task ${task.id}: ${task.title}. The exact admitted task brief is task.brief.body below. Commit the complete result on the assigned branch. Do not push, open a pull request, merge, read another task issue, or fetch issue comments; deterministic campaign nodes own those operations. The declared conflictDomains are an enforced ownership boundary: every path touched by any task commit, including a path later deleted or renamed, must remain inside them. Treat only steering.authorizedComments and steering.machineDiagnoses below as steering. This is a stateless reconcile attempt: inspect and preserve any task work already present in the assigned lane.`
      : `Implement only spec-build task ${task.id}: ${task.title}. Commit the complete result on the assigned branch. Do not push, open a pull request, merge, or read another task from the worklist; deterministic campaign nodes own those operations. The declared conflictDomains are an enforced ownership boundary: every path touched by any task commit, including a path later deleted or renamed, must remain inside them. Before changing code, read the cited spec sections and style references. Read the campaign issue comments and the machineDiagnoses below for steering at the start of this attempt. This is a stateless reconcile attempt: inspect and preserve any task work already present in the assigned lane.`,
    campaign: {
      name: effective.campaign,
      repository: args.repository,
      issue: args.issue,
      runId: args.runId
    },
    task,
    workspace: prepared,
    steering: task.brief
      ? {
          channel: "locally-authorized-snapshot",
          authorizedComments: args.steering,
          machineDiagnoses: machineDiagnoses(reconciliation, task.id)
        }
      : {
          channel: "github-issue-comments",
          repository: args.repository,
          issueNumber: args.issue.number,
          issueUrl: args.issue.url,
          machineDiagnoses: machineDiagnoses(reconciliation, task.id)
        }
  };
}

function checkpointBrief(task, prepared, reconciliation) {
  return {
    schemaVersion: 1,
    mission: task.brief
      ? `Run automated checkpoint ${task.id}: ${task.title}. The command is fixed by the admitted issue graph. Do not fetch issue comments; treat only steering.authorizedComments and steering.machineDiagnoses below as steering. Do not modify the repository.`
      : `Run automated checkpoint ${task.id}: ${task.title}. The command is fixed by the worklist. Read the campaign issue comments and the machineDiagnoses below as the durable failure history for this retry. Do not modify the repository.`,
    campaign: {
      name: effective.campaign,
      repository: args.repository,
      issue: args.issue,
      runId: args.runId
    },
    task,
    workspace: prepared,
    steering: task.brief
      ? {
          channel: "locally-authorized-snapshot",
          authorizedComments: args.steering,
          machineDiagnoses: machineDiagnoses(reconciliation, task.id)
        }
      : {
          channel: "github-issue-comments",
          repository: args.repository,
          issueNumber: args.issue.number,
          issueUrl: args.issue.url,
          machineDiagnoses: machineDiagnoses(reconciliation, task.id)
        }
  };
}

function applyAgentPolicies(spec) {
  if (effective.agent.runtimeMaxSec !== null) {
    spec.runtimeMaxSec = effective.agent.runtimeMaxSec;
  }
  if (effective.agent.approvalPolicy !== null) {
    spec.approvalPolicy = effective.agent.approvalPolicy;
  }
  if (effective.agent.sandboxPolicy !== null) {
    spec.sandboxPolicy = effective.agent.sandboxPolicy;
  }
  return spec;
}

function reconciledProjection(reconciliation) {
  return {
    merged: reconciliation.merged,
    checkpoints: reconciliation.checkpoints,
    remaining: reconciliation.remaining,
    frontier: reconciliation.frontier.map(task => task.id),
    diagnoses: reconciliation.diagnoses,
    blocked: reconciliation.blocked,
    quiescent: reconciliation.quiescent,
    escalation: reconciliation.escalation,
    warnings: reconciliation.warnings
  };
}

async function runGate(task, gate, workspace, prefix) {
  const key = `${prefix}-${task.id}-${gate.id}`;
  const taskRef = taskRefFor(task.id);
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
    taskRef: taskRefFor(task.id)
  });
}

async function sweepCampaign(repositoryConfig) {
  // Producer admission holds the campaign's capacity-1 mutex. Once the prior
  // pass and its admitted children have settled (the documented recovery
  // rule), every other run namespace is stale and can be reclaimed before this
  // pass creates a lane.
  const sweepNode = await driverNode(
    "sweep",
    {
      campaign: effective.campaign,
      repository: args.repository,
      repositoryConfig,
      runId: args.runId,
      workspaceRoot: args.workspaceRoot
    },
    "sweep",
    "spec-build-sweep",
    sweepSchema,
    null,
    false,
    null
  );
  if (sweepNode.disposition !== "created") {
    const error = new Error(
      `spec-build campaign ${effective.campaign} requires a fresh flow-run identity; ` +
        `the sweep node was ${sweepNode.disposition}`
    );
    error.name = "SpecBuildReplayError";
    error.code = "campaign-replay-refused";
    error.details = {
      campaign: effective.campaign,
      disposition: sweepNode.disposition,
      recovery: "post a fresh campaign mention"
    };
    throw error;
  }
  return sweepNode;
}

(async () => {
  const forgeNative = typeof args.worklist === "object";
  campaignTaskIdentity = forgeNative ? args.campaignIdentity : args.campaign;
  if (!forgeNative) {
    const configuredGateIds = [];
    for (const gate of args.gates) {
      if (configuredGateIds.indexOf(gate.id) !== -1) {
        const error = new Error(`campaign gate id ${gate.id} is duplicated`);
        error.name = "SpecBuildConfigurationError";
        error.code = "duplicate-gate-id";
        throw error;
      }
      configuredGateIds.push(gate.id);
    }
  }
  effective = forgeNative
    ? null
    : {
        campaign: args.campaign,
        repositoryConfig: args.repositories[args.repository],
        maxParallel: args.maxParallel,
        agent: args.agent,
        gates: args.gates,
        reconcileCommand: args.reconcileCommand
      };
  let sweepNode = null;
  if (!forgeNative) {
    if (!effective.repositoryConfig) {
      const error = new Error(`campaign repository ${args.repository} is not configured`);
      error.name = "SpecBuildConfigurationError";
      error.code = "repository-not-configured";
      throw error;
    }
    sweepNode = await sweepCampaign(effective.repositoryConfig);
  }
  const reconcileBrief = forgeNative
    ? {
        repository: args.repository,
        issue: args.issue,
        worklist: args.worklist
      }
    : {
        campaign: args.campaign,
        repository: args.repository,
        repositoryConfig: args.repositories[args.repository],
        issue: args.issue,
        worklist: args.worklist,
        maxTasks: args.maxTasks,
        maxParallel: args.maxParallel
      };

  const reconciliationNode = await driverNode(
    "reconcile",
    reconcileBrief,
    "reconcile",
    "spec-build-reconcile",
    reconcileSchema,
    null,
    false,
    null
  );
  const reconciliation = reconciliationNode.result;
  if (forgeNative) {
    effective = reconciliation.config;
  }
  if (!effective || !effective.repositoryConfig) {
    const error = new Error(`campaign repository ${args.repository} is not configured`);
    error.name = "SpecBuildConfigurationError";
    error.code = "repository-not-configured";
    throw error;
  }
  const repositoryConfig = effective.repositoryConfig;
  const gateIds = [];
  for (const gate of effective.gates) {
    if (gateIds.indexOf(gate.id) !== -1) {
      const error = new Error(`campaign gate id ${gate.id} is duplicated`);
      error.name = "SpecBuildConfigurationError";
      error.code = "duplicate-gate-id";
      throw error;
    }
    gateIds.push(gate.id);
  }
  if (forgeNative) {
    sweepNode = await sweepCampaign(repositoryConfig);
  }
  const domainsRequired = effective.maxParallel > 1;

  if (reconciliation.complete) {
    return {
      campaign: effective.campaign,
      repository: args.repository,
      issue: args.issue,
      worklist: reconciliation.source,
      state: "complete",
      reconciled: reconciledProjection(reconciliation),
      maintenance: sweepNode.result,
      checkpoints: [],
      merged: [],
      failures: [],
      diagnoses: [],
      continuation: null,
      escalation: null
    };
  }

  if (reconciliation.quiescent) {
    let escalation = null;
    if (reconciliation.escalation === null) {
      const escalated = await driverNode(
        "escalate",
        reconcileBrief,
        "escalate",
        "spec-build-escalate",
        escalationSchema,
        null,
        false,
        null
      );
      escalation = escalated.result;
    }
    return {
      campaign: effective.campaign,
      repository: args.repository,
      issue: args.issue,
      worklist: reconciliation.source,
      state: "blocked",
      reconciled: reconciledProjection(reconciliation),
      maintenance: sweepNode.result,
      checkpoints: [],
      merged: [],
      failures: [],
      diagnoses: [],
      continuation: null,
      escalation
    };
  }
  // A merged campaign PR is the durable proof that an earlier pass cleared
  // admission. Until that first merge exists, every fresh pass gates a
  // separate pristine-base lane and cleans it before any agent can start.
  const commandGates = effective.gates.filter(gate => gate.kind === "command");
  if (
    !reconciliation.complete &&
    reconciliation.merged.length === 0 &&
    commandGates.length > 0
  ) {
    const preflightTask = reconciliation.frontier.find(task => task.kind === "implementation");
    if (preflightTask !== undefined) {
      const preflightTaskRef = taskRefFor(preflightTask.id);
      const preflight = await driverNode(
        "preflight",
        {
          campaign: effective.campaign,
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
        cleanupBrief(repositoryConfig, "campaign-preflight", preflight.result),
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
  }

  const laneOutcomes = await parallel(
    reconciliation.frontier.map(task => () => (async () => {
      const taskRef = taskRefFor(task.id);
      const prepBrief = {
        campaign: effective.campaign,
        repository: args.repository,
        repositoryConfig,
        issue: args.issue,
        runId: args.runId,
        workspaceRoot: args.workspaceRoot,
        task
      };
      const prepared = await driverNode(
        "prep",
        prepBrief,
        `prep-${task.id}`,
        `prep-${task.id}`,
        workspaceSchema,
        null,
        true,
        taskRef
      );
      if (!nodePassed(prepared)) {
        return {
          task,
          failure: taskFailure(task, "prep", prepared, prepBrief, [], null, null)
        };
      }
      const workspace = workspaceFor(prepared.result);

      if (task.kind === "checkpoint") {
        const taskBrief = checkpointBrief(task, prepared.result, reconciliation);
        const checkpoint = await sh(task.argv, {
          pools: ["campaign-control"],
          priority: "low",
          workspace,
          env: { CAMPAIGN_TASK_ID: task.id },
          runtimeMaxSec: task.runtimeMaxSec,
          evidence: ["exit:0"],
          brief: taskBrief,
          key: `checkpoint-${task.id}`,
          label: `checkpoint-${task.id}`,
          settle: true,
          taskRef
        });
        const gateOutputs = [
          { phase: "checkpoint", gateId: task.id, kind: "checkpoint", node: checkpoint }
        ];
        if (checkpoint.verdict !== "pass") {
          return {
            task,
            prepared: prepared.result,
            failure: taskFailure(
              task,
              "checkpoint",
              checkpoint,
              taskBrief,
              gateOutputs,
              prepared.result,
              prepared.result.baseRev
            )
          };
        }
        const recorded = await driverNode(
          "checkpoint",
          {
            campaign: effective.campaign,
            repository: args.repository,
            repositoryConfig,
            issue: args.issue,
            task,
            source: reconciliation.source,
            workspace: prepared.result
          },
          `checkpoint-record-${task.id}`,
          `checkpoint-record-${task.id}`,
          checkpointCompletionSchema,
          workspace,
          true,
          taskRef
        );
        if (!nodePassed(recorded)) {
          return {
            task,
            prepared: prepared.result,
            failure: taskFailure(
              task,
              "checkpoint:record",
              recorded,
              taskBrief,
              gateOutputs,
              prepared.result,
              prepared.result.baseRev
            )
          };
        }
        return { task, prepared: prepared.result, checkpoint: recorded.result };
      }

      const taskBrief = implementationBrief(task, prepared.result, reconciliation);
      const agentSpec = applyAgentPolicies({
        argv: effective.agent.argv,
        adapter: effective.agent.adapter,
        pools: ["campaign-agent"],
        priority: effective.agent.priority,
        workspace,
        evidence: ["exit:0"],
        brief: taskBrief,
        key: `agent-${task.id}`,
        label: `agent-${task.id}`,
        taskRef
      });
      const agent = await job(agentSpec, { settle: true });
      if (agent.verdict !== "pass") {
        return {
          task,
          prepared: prepared.result,
          failure: taskFailure(
            task,
            "agent",
            agent,
            taskBrief,
            [],
            prepared.result,
            prepared.result.baseRev
          )
        };
      }

      const ownership = await driverNode(
        "ownership",
        {
          task,
          domainsRequired,
          workspace: prepared.result
        },
        `ownership-${task.id}`,
        `ownership-${task.id}`,
        ownershipSchema,
        workspace,
        true,
        taskRef
      );
      if (!nodePassed(ownership)) {
        return {
          task,
          prepared: prepared.result,
          failure: taskFailure(
            task,
            "ownership",
            ownership,
            taskBrief,
            [
              {
                phase: "ownership",
                gateId: "conflict-domains",
                kind: "ownership",
                node: ownership
              }
            ],
            prepared.result,
            prepared.result.baseRev
          )
        };
      }

      const constraintResults = [];
      const gateOutputs = [];
      for (const gate of effective.gates) {
        const gated = await runGate(task, gate, workspace, "gate");
        gateOutputs.push({ phase: "gate", gateId: gate.id, kind: gate.kind, node: gated });
        if (gated.verdict !== "pass") {
          return {
            task,
            prepared: prepared.result,
            failure: taskFailure(
              task,
              `gate:${gate.id}`,
              gated,
              taskBrief,
              gateOutputs,
              prepared.result,
              prepared.result.baseRev
            )
          };
        }
        if (gate.kind === "forbidPaths") {
          constraintResults.push(gated.result);
        }
      }

      const publication = await driverNode(
        "publish",
        {
          campaign: effective.campaign,
          repository: args.repository,
          repositoryConfig,
          issue: args.issue,
          runId: args.runId,
          workspaceRoot: args.workspaceRoot,
          task,
          domainsRequired,
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
          failure: taskFailure(
            task,
            "publish",
            publication,
            taskBrief,
            gateOutputs,
            prepared.result,
            prepared.result.baseRev
          )
        };
      }
      return {
        task,
        prepared: prepared.result,
        publication: publication.result,
        constraints: constraintResults,
        taskBrief,
        gateOutputs
      };
    })()),
    { settle: true }
  );

  const lanes = laneOutcomes.map((outcome, index) => {
    if (outcome.ok) {
      return outcome.value;
    }
    const task = reconciliation.frontier[index];
    return {
      task,
      failure: taskFailure(
        task,
        "lane",
        outcome.error,
        { schemaVersion: 1, task, mission: "Diagnose a failed task lane." },
        [],
        null,
        null
      )
    };
  });
  const failures = lanes.filter(lane => lane.failure).map(lane => lane.failure);
  const checkpoints = lanes.filter(lane => lane.checkpoint).map(lane => lane.checkpoint);
  const publications = lanes.filter(lane => lane.publication);
  const merged = [];

  // Publication work is parallel; integration is deliberately ordered. Before
  // every merge the driver compares the tested base to current main. Only a
  // moved base causes a rebase and a second witnessed gate pass.
  for (const lane of publications) {
    const task = lane.task;
    const taskRef = taskRefFor(task.id);
    const workspace = workspaceFor(lane.prepared);
    const integration = await driverNode(
      "rebase",
      {
        campaign: effective.campaign,
        repository: args.repository,
        repositoryConfig,
        issue: args.issue,
        runId: args.runId,
        workspaceRoot: args.workspaceRoot,
        task,
        domainsRequired,
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
      failures.push(
        taskFailure(
          task,
          "rebase",
          integration,
          lane.taskBrief,
          lane.gateOutputs,
          lane.prepared,
          lane.prepared.baseRev
        )
      );
      continue;
    }

    let regateFailed = false;
    const gateOutputs = lane.gateOutputs.slice();
    if (integration.result.regate) {
      const integratedWorkspace = workspaceFor(lane.prepared, integration.result.baseRev);
      for (const gate of effective.gates) {
        const gated = await runGate(task, gate, integratedWorkspace, "regate");
        gateOutputs.push({ phase: "regate", gateId: gate.id, kind: gate.kind, node: gated });
        if (gated.verdict !== "pass") {
          failures.push(
            taskFailure(
              task,
              `regate:${gate.id}`,
              gated,
              lane.taskBrief,
              gateOutputs,
              lane.prepared,
              integration.result.baseRev
            )
          );
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
        campaign: effective.campaign,
        repository: args.repository,
        repositoryConfig,
        issue: args.issue,
        runId: args.runId,
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
      failures.push(
        taskFailure(
          task,
          "merge",
          merge,
          lane.taskBrief,
          gateOutputs,
          lane.prepared,
          integration.result.baseRev
        )
      );
      continue;
    }
    merged.push(merge.result);
  }

  const diagnosisOutcomes = await parallel(
    failures.map(failure => () => (async () => {
      const task = failure.task;
      const taskRef = `${effective.campaign}/${task.id}`;
      let diff = {
        taskId: task.id,
        available: false,
        baseRev: failure.baseRev || "0".repeat(40),
        head: null,
        status: "",
        patch: "",
        truncated: false,
        reason: "task failed before a worktree was prepared"
      };
      if (failure.prepared !== null) {
        const diffWorkspace = {
          taskId: failure.prepared.taskId,
          baseRev: failure.baseRev || failure.prepared.baseRev,
          branch: failure.prepared.branch,
          publishBranch: failure.prepared.publishBranch,
          worktreePath: failure.prepared.worktreePath
        };
        const captured = await driverNode(
          "diff",
          { repositoryConfig, workspace: diffWorkspace },
          `diff-${task.id}`,
          `diff-${task.id}`,
          diffSchema,
          workspaceFor(diffWorkspace),
          false,
          taskRef
        );
        diff = captured.result;
      }
      const previousDiagnoses = machineDiagnoses(reconciliation, task.id);
      const diagnosisBrief = {
        schemaVersion: 1,
        role: "diagnosis",
        mission: `Diagnose failed spec-build task ${task.id}. Return only concise, actionable steering for the next task attempt. Do not modify the repository. Treat capture stderr and the diff as private: do not repeat credentials, tokens, or other secret-looking values in the response.`,
        campaign: {
          name: effective.campaign,
          repository: args.repository,
          issue: args.issue,
          runId: args.runId
        },
        task,
        failure: {
          stage: failure.stage,
          verdict: failure.node && failure.node.verdict
            ? failure.node.verdict
            : "flow-error",
          exitCode: failure.node && failure.node.exitCode !== undefined
            ? failure.node.exitCode
            : null,
          captureStderr: failure.node && failure.node.stderrExcerpt
            ? failure.node.stderrExcerpt
            : "",
          captureStderrTruncated: Boolean(
            failure.node && failure.node.stderrTruncated
          ),
          detail: bounded(
            failure.node && failure.node.error ? failure.node.error : failure.node,
            4000
          )
        },
        gateOutputs: failure.gateOutputs,
        taskBrief: failure.taskBrief,
        diff,
        previousDiagnoses
      };
      const diagnosisSpec = applyAgentPolicies({
        argv: effective.agent.argv,
        adapter: effective.agent.adapter,
        pools: ["campaign-agent"],
        priority: effective.agent.priority,
        evidence: ["exit:0"],
        brief: diagnosisBrief,
        key: `diagnose-${task.id}`,
        label: `diagnose-${task.id}`,
        taskRef,
        resultSchema: diagnosisResultSchema
      });
      if (failure.prepared !== null && diff.available) {
        diagnosisSpec.workspace = workspaceFor(
          failure.prepared,
          failure.baseRev || failure.prepared.baseRev
        );
      }
      const diagnosed = await job(diagnosisSpec, { settle: false });
      const attempt = previousDiagnoses.length + 1;
      const steering = await driverNode(
        "steer",
        {
          campaign: effective.campaign,
          repository: args.repository,
          repositoryConfig,
          issue: args.issue,
          taskId: task.id,
          attempt,
          diagnosis: diagnosed.result
        },
        `steer-${task.id}`,
        `steer-${task.id}`,
        steeringSchema,
        null,
        false,
        taskRef
      );
      return steering.result;
    })()),
    { settle: true }
  );
  const diagnosisFailure = diagnosisOutcomes.find(outcome => !outcome.ok);
  const diagnoses = diagnosisOutcomes
    .filter(outcome => outcome.ok)
    .map(outcome => outcome.value);
  let terminalError = diagnosisFailure ? diagnosisFailure.error : null;
  if (
    terminalError === null &&
    merged.length === 0 &&
    checkpoints.length === 0 &&
    diagnoses.length === 0
  ) {
    const error = new Error(
      "a non-quiescent campaign frontier produced no merge, checkpoint, or machine steering"
    );
    error.name = "SpecBuildInvariantError";
    error.code = "frontier-without-outcome";
    terminalError = error;
  }

  let continuation = null;
  if (
    terminalError === null &&
    (merged.length > 0 || checkpoints.length > 0 || diagnoses.length > 0) &&
    effective.reconcileCommand !== null
  ) {
    const continued = await driverNode(
      "continue",
      {
        campaign: effective.campaign,
        repository: args.repository,
        repositoryConfig,
        issue: args.issue,
        runId: args.runId,
        reconcileCommand: effective.reconcileCommand
      },
      "continue",
      "spec-build-continue",
      continuationSchema,
      null,
      true,
      null
    );
    if (!nodePassed(continued)) {
      const taskId = merged.length > 0
        ? merged[merged.length - 1].taskId
        : checkpoints.length > 0
          ? checkpoints[checkpoints.length - 1].taskId
          : diagnoses[diagnoses.length - 1].taskId;
      failures.push(failureReport({ id: taskId }, "continuation", continued));
    } else {
      continuation = continued.result;
    }
  }

  const cleanupLanes = reconciliation.frontier.map(task => {
    const lane = lanes.find(candidate => candidate.task.id === task.id);
    return { task, prepared: lane && lane.prepared ? lane.prepared : null };
  });
  const cleanupOutcomes = await parallel(
    cleanupLanes.map(lane => () => driverNode(
      "cleanup",
      cleanupBrief(repositoryConfig, lane.task.id, lane.prepared),
      `cleanup-${lane.task.id}`,
      `cleanup-${lane.task.id}`,
      cleanupSchema,
      null,
      true,
      taskRefFor(lane.task.id)
    )),
    { settle: true }
  );
  for (let index = 0; index < cleanupOutcomes.length; index += 1) {
    const outcome = cleanupOutcomes[index];
    const lane = cleanupLanes[index];
    if (!outcome.ok) {
      failures.push(failureReport(lane.task, "cleanup", outcome.error));
    } else if (!nodePassed(outcome.value)) {
      failures.push(failureReport(lane.task, "cleanup", outcome.value));
    }
  }

  if (terminalError !== null) {
    throw terminalError;
  }

  return {
    campaign: effective.campaign,
    repository: args.repository,
    issue: args.issue,
    worklist: reconciliation.source,
    state: merged.length > 0 || checkpoints.length > 0
      ? "advanced"
      : diagnoses.length > 0
        ? "steered"
        : failures.length > 0
          ? "needs-attention"
          : "idle",
    reconciled: reconciledProjection(reconciliation),
    maintenance: sweepNode.result,
    checkpoints,
    merged,
    failures: failures.map(failure => failure.report || failure),
    diagnoses,
    continuation,
    escalation: null
  };
})();
