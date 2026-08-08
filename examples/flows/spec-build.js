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
    $defs: {
      canonicalArgv: {
        type: "array",
        minItems: 1,
        items: {
          type: "string",
          minLength: 1,
          pattern: "^[^\\u0000-\\u001f\\u007f]+$"
        }
      },
      canonicalAgent: {
        type: "object",
        required: [
          "adapter",
          "argv",
          "priority",
          "runtimeMaxSec",
          "approvalPolicy",
          "sandboxPolicy",
          "diagnosisSandboxPolicy",
          "model"
        ],
        properties: {
          adapter: { type: "string", minLength: 1 },
          argv: { $ref: "#/$defs/canonicalArgv" },
          priority: { enum: ["interrupt", "high", "medium", "low"] },
          runtimeMaxSec: { type: ["integer", "null"], minimum: 1 },
          approvalPolicy: { type: ["string", "null"], minLength: 1 },
          sandboxPolicy: { type: ["string", "null"], minLength: 1 },
          diagnosisSandboxPolicy: { type: ["string", "null"], minLength: 1 },
          model: { type: ["string", "null"], minLength: 1, maxLength: 128 }
        },
        additionalProperties: false
      },
      canonicalSteward: {
        type: ["object", "null"],
        required: ["adapter", "argv", "env", "finalMessagePattern", "runtimeMaxSec"],
        properties: {
          adapter: {
            type: "string",
            minLength: 1,
            maxLength: 80,
            pattern: "^[A-Za-z0-9_][A-Za-z0-9_.-]*$"
          },
          argv: { $ref: "#/$defs/canonicalArgv" },
          env: {
            type: "object",
            maxProperties: 64,
            propertyNames: { pattern: "^[A-Za-z_][A-Za-z0-9_]*$" },
            additionalProperties: { type: "string", minLength: 1, maxLength: 4096 }
          },
          finalMessagePattern: { type: "string", minLength: 1, maxLength: 1024 },
          runtimeMaxSec: { type: ["integer", "null"], minimum: 1 }
        },
        additionalProperties: false
      },
      canonicalGate: {
        oneOf: [
          {
            type: "object",
            required: ["kind", "id", "preflightArgv", "argv", "runtimeMaxSec"],
            properties: {
              kind: { const: "command" },
              id: {
                type: "string",
                maxLength: 80,
                pattern: "^[A-Za-z0-9_][A-Za-z0-9_.-]*$"
              },
              preflightArgv: { $ref: "#/$defs/canonicalArgv" },
              argv: { $ref: "#/$defs/canonicalArgv" },
              runtimeMaxSec: { type: "integer", minimum: 1 }
            },
            additionalProperties: false
          },
          {
            type: "object",
            required: ["kind", "id", "forbidPaths", "runtimeMaxSec"],
            properties: {
              kind: { const: "forbidPaths" },
              id: {
                type: "string",
                maxLength: 80,
                pattern: "^[A-Za-z0-9_][A-Za-z0-9_.-]*$"
              },
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
    },
    required: [
      "repository",
      "issue",
      "runId",
      "worklist",
      "continuation",
      "workspaceRoot",
      "tally",
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
      // The normalized #433 receipt. Current arm dispatches also carry the
      // complete graph below; direct compatibility briefs may use this member
      // with graphDigest and let the driver reconstruct a verified envelope.
      armedManifest: { type: ["object", "null"] },
      // The complete normalized graph Rust admitted and hashed. The flow
      // forwards this versioned envelope unchanged to the packaged driver.
      campaignGraph: {
        type: "object",
        required: ["manifest", "tasks", "executableDigest"],
        properties: {
          manifest: {
            type: "object",
            required: [
              "schemaVersion",
              "name",
              "repository",
              "maxTasks",
              "maxParallel",
              "driverRuntimeMaxSec",
              "runtimeMaxSec",
              "pool",
              "mergeMethod",
              "gitAiBinding",
              "gitAiAwaitSec",
              "agent",
              "steward",
              "gates",
              "tasks"
            ],
            properties: {
              schemaVersion: { const: 1 },
              name: {
                type: "string",
                maxLength: 80,
                pattern: "^[A-Za-z0-9_][A-Za-z0-9_.-]*$"
              },
              repository: {
                type: "object",
                required: ["checkout", "baseBranch", "remote", "forge"],
                properties: {
                  checkout: { type: "string", pattern: "^/" },
                  baseBranch: { type: "string", minLength: 1 },
                  remote: {
                    type: "string",
                    maxLength: 80,
                    pattern: "^[A-Za-z0-9_][A-Za-z0-9_.-]*$"
                  },
                  forge: { enum: ["github", "local"] }
                },
                additionalProperties: false
              },
              maxTasks: { type: "integer", minimum: 1, maximum: 100 },
              maxParallel: { type: "integer", minimum: 1, maximum: 100 },
              driverRuntimeMaxSec: { type: "integer", minimum: 1 },
              runtimeMaxSec: { type: ["integer", "null"], minimum: 1 },
              pool: {
                type: "string",
                maxLength: 80,
                pattern: "^[A-Za-z0-9_][A-Za-z0-9_.-]*$"
              },
              mergeMethod: { enum: ["merge", "squash"] },
              gitAiBinding: { enum: ["off", "advisory", "required"] },
              gitAiAwaitSec: { type: "integer", minimum: 1 },
              agent: { $ref: "#/$defs/canonicalAgent" },
              steward: { $ref: "#/$defs/canonicalSteward" },
              gates: {
                type: "array",
                minItems: 1,
                maxItems: 16,
                items: { $ref: "#/$defs/canonicalGate" }
              },
              tasks: {
                type: "array",
                minItems: 1,
                maxItems: 100,
                items: {
                  type: "object",
                  required: [
                    "id",
                    "kind",
                    "issue",
                    "dependencies",
                    "argv",
                    "runtimeMaxSec"
                  ],
                  properties: {
                    id: { type: "string" },
                    kind: { enum: ["implementation", "checkpoint"] },
                    issue: { type: "integer" },
                    dependencies: { type: "array" },
                    conflictDomains: { type: "array" },
                    argv: { type: ["array", "null"] },
                    runtimeMaxSec: { type: ["integer", "null"] }
                  },
                  additionalProperties: false
                }
              }
            },
            additionalProperties: false
          },
          tasks: {
            type: "array",
            minItems: 1,
            maxItems: 100,
            items: {
              type: "object",
              required: ["number", "title", "body"],
              properties: {
                number: { type: "integer", minimum: 1 },
                title: { type: "string", minLength: 1, maxLength: 300 },
                body: { type: "string", minLength: 1, maxLength: 64000 }
              },
              additionalProperties: false
            }
          },
          executableDigest: { type: "string", pattern: "^sha256:[0-9a-f]{64}$" }
        },
        additionalProperties: false
      },
      repository: { type: "string", pattern: "^[^/ \\t]+/[^/ \\t]+$" },
      // The two-repository seam. Each names an entry of `repositories`. A
      // campaign that sets none of them resolves every coordinate to
      // `repository` and runs the single-repository path unchanged.
      codeRepository: { type: "string", pattern: "^[^/ \\t]+/[^/ \\t]+$" },
      specRepository: { type: "string", pattern: "^[^/ \\t]+/[^/ \\t]+$" },
      issueRepository: { type: "string", pattern: "^[^/ \\t]+/[^/ \\t]+$" },
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
      // The machine's self-continuation. A pass that advanced writes this
      // bounded enqueue payload into the daemon's events directory; the 5 s
      // drain admits the next pass. No forge round-trip, no public comment.
      continuation: {
        type: "object",
        required: ["argv", "pool", "priority", "eventsDir"],
        properties: {
          argv: {
            type: "array",
            minItems: 1,
            maxItems: 64,
            items: { type: "string", minLength: 1, pattern: "^[^\\u0000-\\u001f\\u007f]+$" }
          },
          pool: {
            type: "array",
            minItems: 1,
            maxItems: 8,
            uniqueItems: true,
            items: { type: "string", minLength: 1, maxLength: 128 }
          },
          priority: { enum: ["interrupt", "high", "medium", "low"] },
          runtimeMaxSec: { type: ["integer", "null"], minimum: 1 },
          eventsDir: { type: "string", pattern: "^/" }
        },
        additionalProperties: false
      },
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
      // Human steering collected from each task's own sub-issue thread, keyed
      // by sub-issue number. The master stays the campaign-wide channel and
      // still reaches every task; a task thread reaches exactly one.
      taskSteering: {
        type: "object",
        maxProperties: 100,
        additionalProperties: {
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
        }
      },
      // What the arm-time probe found this forge can serve. Absent means
      // degraded: checkbox projection, per-branch pull-request lookup.
      capabilities: {
        type: "object",
        required: ["subIssueWalk"],
        properties: {
          subIssueWalk: { type: "boolean" }
        },
        additionalProperties: false
      },
      // How the merge node integrates a task. Absent means the campaign
      // default, `squash`: the footprint a campaign should leave behind is one
      // conventional commit per task, not a merge commit with a template message.
      mergeMethod: { enum: ["merge", "squash"] },
      // Whether the merge node binds Git AI authorship on the commit it
      // integrated. Absent means `off`: the shipped state binds nothing.
      gitAiBinding: { enum: ["off", "advisory", "required"] },
      // How long the merge node may wait on git-ai's settlement barrier. The
      // module derives it from this campaign's own node deadline; absent is
      // the driver's default.
      gitAiAwaitSec: { type: "integer", minimum: 1 },
      // Both module-declared and forge-native paths carry the normalized
      // contract; the driver never fills these members in.
      steward: { $ref: "#/$defs/canonicalSteward" },
      agent: { $ref: "#/$defs/canonicalAgent" },
      gates: {
        type: "array",
        minItems: 1,
        maxItems: 16,
        uniqueItems: true,
        items: { $ref: "#/$defs/canonicalGate" }
      }
    },
    oneOf: [
      {
        required: [
          "campaign",
          "repositories",
          "maxTasks",
          "maxParallel",
          "agent",
          "gates"
        ]
      },
      {
        required: ["campaignIdentity", "campaignGraph", "steering", "tally"],
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
  // first merge, a separate pristine-base lane preflights every command gate --
  // and witnesses each gate's real argv, non-gating, beside its probe -- before
  // the dependency-ready implementation frontier is admitted.
  iterationCap: 4096,
  selectors: []
};

const canonicalArgvSchema = meta.argsSchema.$defs.canonicalArgv;
const canonicalAgentDefinition = meta.argsSchema.$defs.canonicalAgent;
const canonicalCampaignAgentSchema = {
  ...canonicalAgentDefinition,
  properties: { ...canonicalAgentDefinition.properties, argv: canonicalArgvSchema }
};
const canonicalStewardDefinition = meta.argsSchema.$defs.canonicalSteward;
const canonicalCampaignStewardSchema = {
  ...canonicalStewardDefinition,
  properties: { ...canonicalStewardDefinition.properties, argv: canonicalArgvSchema }
};
const [canonicalCommandGateDefinition, canonicalForbidGateDefinition] =
  meta.argsSchema.$defs.canonicalGate.oneOf;
const canonicalCampaignGateSchema = {
  oneOf: [
    {
      ...canonicalCommandGateDefinition,
      properties: {
        ...canonicalCommandGateDefinition.properties,
        preflightArgv: canonicalArgvSchema,
        argv: canonicalArgvSchema
      }
    },
    canonicalForbidGateDefinition
  ]
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
    "dependencies"
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
  required: ["id", "kind", "title", "brief", "dependencies", "revision"],
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
        revision: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
        // Present only when the worklist was read from a spec repository
        // that is not the repository the campaign lands its work on.
        repository: { type: "string", pattern: "^[^/ \\t]+/[^/ \\t]+$" }
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
    // New receipts land in the hidden state namespace; already-published
    // visible tag receipts stay honored.
    ref: { type: "string", pattern: "^refs/(tags/)?tally/spec-build/v1/" },
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
    "mergeMethod",
    "gitAiBinding",
    "gitAiAwaitSec",
    "agent",
    "gates"
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
    mergeMethod: { enum: ["merge", "squash"] },
    gitAiBinding: { enum: ["off", "advisory", "required"] },
    gitAiAwaitSec: { type: "integer", minimum: 1 },
    agent: canonicalCampaignAgentSchema,
    steward: canonicalCampaignStewardSchema,
    gates: {
      type: "array",
      minItems: 1,
      maxItems: 16,
      items: canonicalCampaignGateSchema
    }
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

const retryFactSchema = {
  type: "object",
  required: ["taskId", "attempt", "comment", "reason"],
  properties: {
    taskId: taskIdSchema,
    attempt: { type: "integer", minimum: 1, maximum: 2 },
    comment: { type: "string", minLength: 1 },
    reason: { type: "string", minLength: 1, maxLength: 2000 }
  },
  additionalProperties: false
};

const deferralFactSchema = {
  type: "object",
  required: ["taskId", "waitingOn"],
  properties: {
    taskId: taskIdSchema,
    waitingOn: {
      type: "array",
      minItems: 1,
      uniqueItems: true,
      items: taskIdSchema
    }
  },
  additionalProperties: false
};

// A sub-issue closed with no revision-valid merged pull request. Closure is
// human-clickable and therefore proves nothing; the task stays incomplete and
// the closure is surfaced loudly instead of being filed as a warning.
const anomalyFactSchema = {
  type: "object",
  required: ["kind", "taskId", "issue", "url", "detail"],
  properties: {
    kind: { const: "closed-without-merged-proof" },
    taskId: taskIdSchema,
    issue: { type: "string", pattern: "^[1-9][0-9]*$" },
    url: { type: "string", minLength: 1 },
    detail: { type: "string", minLength: 1, maxLength: 2000 }
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
    "baseRevision",
    "tasks",
    "merged",
    "checkpoints",
    "remaining",
    "frontier",
    "diagnoses",
    "retries",
    "deferrals",
    "blocked",
    "quiescent",
    "escalation",
    "complete",
    "anomalies",
    "warnings",
    "closingSummary"
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
    // The code revision this pass reasoned from: the worklist revision for a
    // single-repository campaign, the code repository's base tip for a split.
    baseRevision: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
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
    retries: { type: "array", maxItems: 256, items: retryFactSchema },
    deferrals: { type: "array", maxItems: 128, items: deferralFactSchema },
    blocked: { type: "array", maxItems: 128, items: blockedFactSchema },
    quiescent: { type: "boolean" },
    escalation: { type: ["string", "null"], minLength: 1 },
    complete: { type: "boolean" },
    anomalies: { type: "array", maxItems: 128, items: anomalyFactSchema },
    warnings: stringList,
    // Where the completion path published this campaign's closing summary, or
    // null on any pass that was not the terminal one.
    closingSummary: { type: ["string", "null"], minLength: 1 },
    config: effectiveConfigSchema
  },
  additionalProperties: false
};

const sweepSchema = {
  type: "object",
  required: ["currentRunHash", "blockingJobs", "cleaned", "liveRuns", "warnings"],
  properties: {
    currentRunHash: { type: "string", pattern: "^[0-9a-f]{12}$" },
    blockingJobs: {
      type: "array",
      items: {
        type: "object",
        required: ["anchor", "flowRunId", "liveState", "taskRef"],
        properties: {
          anchor: { type: "string", minLength: 1 },
          flowRunId: { type: "string", minLength: 1 },
          liveState: { enum: ["paused", "queued", "running"] },
          taskRef: { type: "string", minLength: 1 }
        },
        additionalProperties: false
      }
    },
    cleaned: stringList,
    liveRuns: {
      type: "array",
      items: {
        type: "object",
        required: ["runHash", "flowRunId", "jobs"],
        properties: {
          runHash: { type: "string", pattern: "^[0-9a-f]{12}$" },
          flowRunId: { type: "string", minLength: 1 },
          jobs: {
            type: "array",
            minItems: 1,
            items: {
              type: "object",
              required: ["anchor", "liveState", "taskRef"],
              properties: {
                anchor: { type: "string", minLength: 1 },
                liveState: { enum: ["paused", "queued", "running"] },
                taskRef: { type: ["string", "null"] }
              },
              additionalProperties: false
            }
          }
        },
        additionalProperties: false
      }
    },
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

// #386: the tree-delta permission gate's own result. `allowlistBasis` names
// which of the three documented allowlist derivations governed this task, so
// a reader of the witnessed result never has to guess whether an absent
// `conflictDomains` fell back to something permissive. #424: `ownershipRan`
// names which of the gate's two call sites produced the verdict -- after a
// passing agent and its ownership node, or in place of ownership on a pass
// whose agent failed -- so a reader can tell whether an `ownedPaths` fallback
// was even available, and can see that a failed pass was in fact judged.
const treeDeltaSchema = {
  type: "object",
  required: [
    "taskId",
    "checkedPaths",
    "allowlistBasis",
    "allowlist",
    "ownershipRan"
  ],
  properties: {
    taskId: taskIdSchema,
    checkedPaths: { type: "integer", minimum: 0 },
    allowlistBasis: {
      enum: ["declared", "declared-empty", "owned-paths-fallback"]
    },
    allowlist: {
      type: "array",
      uniqueItems: true,
      items: { type: "string", minLength: 1 }
    },
    ownershipRan: { type: "boolean" }
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
    // New receipts land in the hidden state namespace; already-published
    // visible tag receipts stay honored.
    ref: { type: "string", pattern: "^refs/(tags/)?tally/spec-build/v1/" },
    revision: { type: "string", pattern: "^[0-9a-f]{40,64}$" }
  },
  additionalProperties: false
};

// The validated publication text: what the pull request says and what the
// squash commit will say. `source` records whether the steward authored it or
// the deterministic template did.
const narrationSchema = {
  type: "object",
  required: ["source", "subject", "body"],
  properties: {
    source: { enum: ["steward", "template"] },
    subject: { type: "string", minLength: 1, maxLength: 200 },
    body: { type: "string", maxLength: 4000 }
  },
  additionalProperties: false
};

// The validator transcript. Observability only: it is journaled with the
// publish node's result and never reaches the forge.
const narrationAttemptsSchema = {
  type: "array",
  maxItems: 2,
  items: {
    type: "object",
    required: ["attempt", "status", "reason"],
    properties: {
      attempt: { type: "integer", minimum: 1, maximum: 2 },
      status: { enum: ["accepted", "rejected", "failed"] },
      reason: { type: ["string", "null"], maxLength: 200 }
    },
    additionalProperties: false
  }
};

const publicationSchema = {
  type: "object",
  required: [
    "taskId",
    "branch",
    "head",
    "pullRequest",
    "narration",
    "narrationAttempts",
    "ownership"
  ],
  properties: {
    taskId: taskIdSchema,
    branch: { type: "string", minLength: 1 },
    head: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    pullRequest: { type: "string", minLength: 1 },
    narration: narrationSchema,
    narrationAttempts: narrationAttemptsSchema,
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
    "narration",
    "regate",
    "ownership"
  ],
  properties: {
    taskId: taskIdSchema,
    baseRev: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    branch: { type: "string", minLength: 1 },
    head: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    pullRequest: { type: "string", minLength: 1 },
    narration: narrationSchema,
    regate: { type: "boolean" },
    ownership: ownershipSchema
  },
  additionalProperties: false
};

// What the post-merge Git AI binding found, journaled with the merge node.
// It is a receipt, never a gate: under `advisory` every status other than
// `bound` is an observable warning and the campaign continues.
const authorshipReceiptSchema = {
  type: ["object", "null"],
  required: ["binding", "status", "revision", "noteRef", "published", "reason"],
  properties: {
    binding: { enum: ["advisory", "required"] },
    // Exactly what `bind_authorship` can settle on. `conflict` is the remote
    // already carrying a different authorship record for this revision, which
    // is refused rather than merged.
    status: {
      enum: ["bound", "unavailable", "missing-note", "mismatch", "conflict", "error"]
    },
    revision: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    noteRef: { const: "refs/notes/ai" },
    // The campaign remote's refs/notes/ai after publication -- what a reader
    // fetching the notes ref will resolve, not what the checkout holds.
    notesRefTarget: { type: ["string", "null"], pattern: "^[0-9a-f]{40,64}$" },
    noteSha256: { type: ["string", "null"], pattern: "^sha256:[0-9a-f]{64}$" },
    published: { type: "boolean" },
    reason: { type: ["string", "null"], maxLength: 400 }
  },
  additionalProperties: false
};

const mergeSchema = {
  type: "object",
  required: [
    "taskId",
    "head",
    "mergeCommit",
    "pullRequest",
    "regated",
    "ownership",
    "authorship",
    "trailer"
  ],
  properties: {
    taskId: taskIdSchema,
    head: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    mergeCommit: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    pullRequest: { type: "string", minLength: 1 },
    regated: { type: "boolean" },
    ownership: ownershipSchema,
    authorship: authorshipReceiptSchema,
    // The exact `Assisted-by:` line the node wrote into the squash message,
    // or null when the campaign could not name the assisting session. The
    // trailer is a pointer; the note is the proof.
    trailer: { type: ["string", "null"], maxLength: 400 }
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
  required: ["posted", "comment", "summary", "diagnosisCount", "retryCount"],
  properties: {
    posted: { type: "boolean" },
    comment: { type: "string", minLength: 1 },
    // The closing summary posted beside the escalation, reflecting partial
    // state. Null only when this pass found an escalation already posted.
    summary: { type: ["string", "null"], minLength: 1 },
    diagnosisCount: { type: "integer", minimum: 1, maximum: 256 },
    retryCount: { type: "integer", minimum: 0, maximum: 256 }
  },
  additionalProperties: false
};

const continuationSchema = {
  type: "object",
  required: ["event", "dedupKey", "runId", "created"],
  properties: {
    event: { type: "string", pattern: "^/" },
    dedupKey: { type: "string", minLength: 1, maxLength: 512 },
    runId: { type: "string", minLength: 1, maxLength: 512 },
    // False when an identical, not-yet-drained event is already queued. The
    // pass still advanced; a second identical file would only be collapsed by
    // the enqueue kernel, so refusing to write it is the same outcome sooner.
    created: { type: "boolean" },
    receipt: { type: ["string", "null"] }
  },
  additionalProperties: false
};

const retrySchema = {
  type: "object",
  required: ["taskId", "attempt", "comment", "exhausted", "posted", "redacted"],
  properties: {
    taskId: taskIdSchema,
    attempt: { type: "integer", minimum: 0, maximum: 2 },
    comment: { type: ["string", "null"], minLength: 1 },
    exhausted: { type: "boolean" },
    posted: { type: "boolean" },
    redacted: { type: "boolean" }
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

// The campaign's three repository coordinates, resolved once. `code` is where
// lanes, publish branches, pull requests and merges live; `spec` is where the
// worklist artifact is read from; `issue` is where the campaign thread and
// every machine receipt live. Each defaults inward -- issue to spec, spec and
// code to the repository the campaign issue was read from -- so a campaign
// that names none of them resolves all three to the same repository and takes
// exactly the pre-seam path.
const codeRepository = args.codeRepository || args.repository;
const specRepository = args.specRepository || args.repository;
const issueRepository = args.issueRepository || specRepository;
const seamSplit =
  specRepository !== codeRepository || issueRepository !== codeRepository;

function repositoryConfigFor(repository) {
  const configured = args.repositories === undefined ? undefined : args.repositories[repository];
  if (!configured) {
    const error = new Error(`campaign repository ${repository} is not configured`);
    error.name = "SpecBuildConfigurationError";
    error.code = "repository-not-configured";
    throw error;
  }
  return configured;
}

// The seam block a brief carries. Omitted entirely when the campaign is
// single-repository, so its briefs -- and therefore their payload hashes --
// are byte-identical to the ones minted before the seam existed.
function withSeam(brief) {
  if (!seamSplit) {
    return brief;
  }
  return {
    ...brief,
    specRepository: {
      repository: specRepository,
      repositoryConfig: repositoryConfigFor(specRepository)
    },
    issueRepository: {
      repository: issueRepository,
      repositoryConfig: repositoryConfigFor(issueRepository)
    }
  };
}

function taskRefFor(taskId) {
  return `${campaignTaskIdentity}/${taskId}`;
}

function workspaceFor(prepared, baseRev) {
  return {
    repo: codeRepository,
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
    repository: codeRepository,
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

// A campaign has two unrelated kinds of failure and must not price them the
// same. A red gate, a rejected ownership boundary, an agent that exits non-zero
// and a red checkpoint command are all evidence that the task's work is wrong,
// and each one spends one of the task's two steering attempts. Preparing a
// worktree, rebasing, publishing and merging are campaign machinery: when they
// fault they say nothing about the work, so they buy a bounded forge-counted
// retry instead. A checkpoint that runs while unrelated implementation work is
// still outstanding is neither - its verdict is not yet meaningful.
function failureClass(reconciliation, failure) {
  const stage = failure.stage;
  // The whole deferred lane is unpriced, not just its checkpoint command. A
  // checkpoint lane fails at three stages -- `prep`, `checkpoint`, and
  // `checkpoint:record` -- and matching only the middle one left the other two
  // buying a machinery retry and then a steering attempt out of the budget of
  // a task the reconciler has just said has no meaningful verdict yet. The
  // #308 loop bound terminates by spending that budget on attempts that mean
  // something; spending it on passes where the checkpoint could not have
  // settled either way escalates a checkpoint that was never really run. The
  // deferral set only ever names checkpoints, and it drains as unrelated work
  // merges or blocks, so nothing here can defer for ever.
  if (
    failure.task.kind === "checkpoint" &&
    reconciliation.deferrals.some(item => item.taskId === failure.task.id)
  ) {
    return "deferred";
  }
  // #386: an out-of-allowlist tree delta is a breach, not a gate-fail -- the
  // write already happened, so it is not redoable the way a red gate is.
  // Routed separately from "work" so it never buys a retried dispatch; see
  // the `steerable`/breach-tagging split below, which posts it through the
  // existing diagnosis ledger already blocked at attempt 2.
  if (stage === "treeDelta") {
    return "breach";
  }
  // #424: the gate refusing to judge a pass is not the same event as the gate
  // catching a write, and must not be posted under the other one's sentence.
  // It is priced the same -- a gate verdict, never the agent's fault, never a
  // steering attempt -- and it aborts the lane for the same reason: nothing
  // downstream can certify a worktree no allowlist covers.
  if (stage === "treeDelta:ungated") {
    return "ungated";
  }
  if (
    stage === "agent" ||
    stage === "ownership" ||
    stage === "checkpoint" ||
    stage.startsWith("gate:") ||
    stage.startsWith("regate:")
  ) {
    return "work";
  }
  return "machinery";
}

function implementationBrief(task, prepared, reconciliation) {
  const ownershipBoundary = !Array.isArray(task.conflictDomains)
    ? "This serial task omits conflictDomains. Ownership will certify its committed paths, and the tree-delta gate will allow exactly those owned paths after ownership runs."
    : task.conflictDomains.length === 0
      ? "The declared conflictDomains list is explicitly empty, so the task may change no path."
      : "The declared conflictDomains are an enforced ownership boundary: every path touched by any task commit, including a path later deleted or renamed, must remain inside them.";
  return {
    schemaVersion: 1,
    mission: task.brief
      ? `Implement only forge task ${task.id}: ${task.title}. The exact admitted task brief is task.brief.body below. Commit the complete result on the assigned branch. Do not push, open a pull request, merge, read another task issue, or fetch issue comments; deterministic campaign nodes own those operations. ${ownershipBoundary} Treat only steering.authorizedComments and steering.machineDiagnoses below as steering. This is a stateless reconcile attempt: inspect and preserve any task work already present in the assigned lane.`
      : `Implement only spec-build task ${task.id}: ${task.title}. Commit the complete result on the assigned branch. Do not push, open a pull request, merge, or read another task from the worklist; deterministic campaign nodes own those operations. ${ownershipBoundary} Before changing code, read the cited spec sections and style references. Read the campaign issue comments and the machineDiagnoses below for steering at the start of this attempt. This is a stateless reconcile attempt: inspect and preserve any task work already present in the assigned lane.`,
    campaign: {
      name: effective.campaign,
      repository: codeRepository,
      issue: args.issue,
      runId: args.runId
    },
    task,
    workspace: prepared,
    steering: task.brief
      ? {
          channel: "locally-authorized-snapshot",
          authorizedComments: authorizedComments(task),
          machineDiagnoses: machineDiagnoses(reconciliation, task.id)
        }
      : {
          // The steering channel is the campaign thread, which is not
          // necessarily the repository the agent is working in.
          channel: "github-issue-comments",
          repository: issueRepository,
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
      repository: codeRepository,
      issue: args.issue,
      runId: args.runId
    },
    task,
    workspace: prepared,
    steering: task.brief
      ? {
          channel: "locally-authorized-snapshot",
          authorizedComments: authorizedComments(task),
          machineDiagnoses: machineDiagnoses(reconciliation, task.id)
        }
      : {
          // The steering channel is the campaign thread, which is not
          // necessarily the repository the agent is working in.
          channel: "github-issue-comments",
          repository: issueRepository,
          issueNumber: args.issue.number,
          issueUrl: args.issue.url,
          machineDiagnoses: machineDiagnoses(reconciliation, task.id)
        }
  };
}

// The diagnosis brief prohibits mutation. An operator whose adapter declares no
// read-only policy sets diagnosisSandboxPolicy to null explicitly.
function diagnosisSandboxed(agent) {
  if (agent.diagnosisSandboxPolicy !== undefined) {
    return agent;
  }
  return { ...agent, diagnosisSandboxPolicy: "read-only" };
}

function applyAgentPolicies(spec, sandboxPolicy = effective.agent.sandboxPolicy) {
  if (effective.agent.runtimeMaxSec !== null) {
    spec.runtimeMaxSec = effective.agent.runtimeMaxSec;
  }
  if (effective.agent.model !== null && effective.agent.model !== undefined) {
    spec.model = effective.agent.model;
  }
  if (effective.agent.approvalPolicy !== null) {
    spec.approvalPolicy = effective.agent.approvalPolicy;
  }
  if (sandboxPolicy !== null && sandboxPolicy !== undefined) {
    spec.sandboxPolicy = sandboxPolicy;
  }
  return spec;
}

// The arm-time capability record travels with every brief that reads or
// writes a forge surface, so one pass never mixes native and degraded
// projections. Absent means degraded.
function withCapabilities(brief) {
  if (args.capabilities === undefined) {
    return brief;
  }
  return { ...brief, capabilities: args.capabilities };
}

function nativeSubIssues() {
  return args.capabilities !== undefined && args.capabilities.subIssueWalk === true;
}

// Task T's machine receipts belong on T's own sub-issue thread. Without that
// capability, or without a sub-issue, they stay on the master.
function taskThread(task) {
  if (!nativeSubIssues() || !task.brief) {
    return null;
  }
  return task.brief.issue;
}

// The master reaches every task; a task's own sub-issue thread reaches only
// that task.
function authorizedComments(task) {
  const master = args.steering || [];
  const thread = taskThread(task);
  if (thread === null || args.taskSteering === undefined) {
    return master;
  }
  return master.concat(args.taskSteering[thread.number] || []);
}

function reconciledProjection(reconciliation) {
  return {
    anomalies: reconciliation.anomalies,
    merged: reconciliation.merged,
    checkpoints: reconciliation.checkpoints,
    remaining: reconciliation.remaining,
    frontier: reconciliation.frontier.map(task => task.id),
    diagnoses: reconciliation.diagnoses,
    retries: reconciliation.retries,
    deferrals: reconciliation.deferrals,
    blocked: reconciliation.blocked,
    quiescent: reconciliation.quiescent,
    escalation: reconciliation.escalation,
    closingSummary: reconciliation.closingSummary,
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
      // The gate resolves the lane's own history against the current base
      // branch, so it needs the repository the campaign is configured with.
      repositoryConfig: effective.repositoryConfig,
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

// The gating probe above is only ever a proxy: `preflightArgv` is declared
// base-safe, and nothing validates that it is representative of the argv that
// actually decides a merge. So after a gate's proxy passes, run the real
// merge-criterion `argv` once on the same pristine base, same workspace, same
// CAMPAIGN_TASK_ID, same deadline -- and gate on nothing. No `exit:0` evidence
// is declared, the verdict is discarded, and the pass continues whatever
// happens; the value is the witness record and the capture files, which carry
// the exact argv, exit code, and stderr from the exact host at t=0. An
// estate-side toolchain defect that the proxy cannot see becomes visible before
// the first agent cycle, while a base that is legitimately red until an agent
// builds something stays tolerated.
async function runPreflightWitness(task, gate, workspace) {
  return sh(gate.argv, {
    pools: ["campaign-control"],
    priority: "low",
    workspace,
    env: { CAMPAIGN_TASK_ID: task.id },
    runtimeMaxSec: gate.runtimeMaxSec,
    key: `preflight-witness-${gate.id}`,
    label: `preflight-witness-${gate.id}`,
    settle: true,
    taskRef: taskRefFor(task.id)
  });
}

async function sweepCampaign(repositoryConfig) {
  // Producer admission holds the campaign's capacity-1 mutex only for the
  // runner process. The sweep registers this pass's run hash against its
  // daemon flow identity and proves that every older flow has no live child
  // before reclaiming its namespace. A killed runner may release the mutex
  // before an admitted child settles, so prose or process liveness is not a
  // deletion proof.
  const sweepNode = await driverNode(
    "sweep",
    {
      campaign: effective.campaign,
      campaignIdentity: campaignTaskIdentity,
      repository: codeRepository,
      repositoryConfig,
      runId: args.runId,
      workspaceRoot: args.workspaceRoot,
      tally: args.tally
    },
    "sweep",
    "spec-build-sweep",
    sweepSchema,
    null,
    false,
    null
  );
  if (sweepNode.disposition === "reused") {
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

function sweepDeferral(sweepNode) {
  if (sweepNode.result.blockingJobs.length === 0 && sweepNode.result.liveRuns.length === 0) {
    return null;
  }
  return {
    campaign: effective.campaign,
    repository: codeRepository,
    issue: args.issue,
    state: "deferred-live-jobs",
    maintenance: sweepNode.result,
    checkpoints: [],
    merged: [],
    failures: []
  };
}

(async () => {
  const forgeNative = typeof args.worklist === "object";
  if (forgeNative && seamSplit) {
    // A forge-native campaign *is* its issue: worklist, briefs and receipts
    // are all that one thread, so there is no second repository to bind.
    const error = new Error("a forge-native campaign cannot span repositories");
    error.name = "SpecBuildConfigurationError";
    error.code = "forge-native-two-repo";
    throw error;
  }
  if (seamSplit) {
    // Fail before the first node rather than at the first read.
    repositoryConfigFor(codeRepository);
    repositoryConfigFor(specRepository);
    repositoryConfigFor(issueRepository);
  }
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
        repositoryConfig: args.repositories[codeRepository],
        maxParallel: args.maxParallel,
        mergeMethod: args.mergeMethod || "squash",
        gitAiBinding: args.gitAiBinding || "off",
        gitAiAwaitSec: args.gitAiAwaitSec || 60,
        agent: diagnosisSandboxed(args.agent),
        steward: args.steward || null,
        gates: args.gates
      };
  let sweepNode = null;
  if (!forgeNative) {
    if (!effective.repositoryConfig) {
      const error = new Error(`campaign repository ${codeRepository} is not configured`);
      error.name = "SpecBuildConfigurationError";
      error.code = "repository-not-configured";
      throw error;
    }
    sweepNode = await sweepCampaign(effective.repositoryConfig);
    const deferred = sweepDeferral(sweepNode);
    if (deferred !== null) {
      return deferred;
    }
  }
  const reconcileBrief = forgeNative
    ? withCapabilities({
        repository: codeRepository,
        issue: args.issue,
        worklist: args.worklist,
        // Forward the already-normalized executable contract unchanged.
        campaignGraph: args.campaignGraph,
        // Preserve the additive receipt evidence when the producer carried
        // it; absence remains absence for briefs armed before #433.
        ...(args.armedManifest === undefined
          ? {}
          : { armedManifest: args.armedManifest })
      })
    : withSeam({
        campaign: args.campaign,
        repository: codeRepository,
        repositoryConfig: args.repositories[codeRepository],
        issue: args.issue,
        worklist: args.worklist,
        maxTasks: args.maxTasks,
        maxParallel: args.maxParallel
      });
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
    const error = new Error(`campaign repository ${codeRepository} is not configured`);
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
    const deferred = sweepDeferral(sweepNode);
    if (deferred !== null) {
      return deferred;
    }
  }
  const domainsRequired = effective.maxParallel > 1;

  if (reconciliation.complete) {
    return {
      campaign: effective.campaign,
      repository: codeRepository,
      issue: args.issue,
      worklist: reconciliation.source,
      state: "complete",
      reconciled: reconciledProjection(reconciliation),
      maintenance: sweepNode.result,
      checkpoints: [],
      merged: [],
      failures: [],
      diagnoses: [],
      retries: [],
      deferrals: [],
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
      repository: codeRepository,
      issue: args.issue,
      worklist: reconciliation.source,
      state: "blocked",
      reconciled: reconciledProjection(reconciliation),
      maintenance: sweepNode.result,
      checkpoints: [],
      merged: [],
      failures: [],
      diagnoses: [],
      retries: [],
      deferrals: [],
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
          repository: codeRepository,
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
      // Every gating probe first, then every witness. The two loops share one
      // worktree and must not be interleaved: a probe is declared base-safe, a
      // gate's real `argv` is the merge criterion and is expected to build and
      // write. Running a witness between two probes hands the second probe a
      // base the first gate's argv has already mutated, so a probe that asserts
      // its own subject is absent on the base -- the shape this repository's own
      // fixtures and examples teach -- goes red because of an unrelated gate,
      // and the pass refuses admission naming the innocent gate. Probes see the
      // pristine base; witnesses see the base plus whatever earlier witnesses
      // did, which is the same order the post-change gate sequence has always
      // run in.
      for (const gate of commandGates) {
        const gated = await runPreflightGate(preflightTask, gate, preflightWorkspace);
        if (gated.verdict !== "pass") {
          failedGate = { gate, node: gated };
          break;
        }
      }
      if (failedGate === null) {
        for (const gate of commandGates) {
          // Non-gating: its verdict is never read, so a red real argv on the
          // pristine base is recorded and the pass proceeds to agent dispatch.
          // Witnesses run only where every probe passed, because a red proxy has
          // already stopped the pass.
          await runPreflightWitness(preflightTask, gate, preflightWorkspace);
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

  // A checkpoint reads the accumulated tree, so it is executed after this
  // pass's own merges rather than beside them. Sharing a frontier with a
  // mergeable implementation task used to guarantee waste: the checkpoint
  // recorded a receipt against the pre-merge base and the pass then moved that
  // base out from under it, so the next reconcile found nothing and ran the
  // whole checkpoint again. Prepared after the merges, the tested revision is
  // the one the next pass reconciles.
  const laneFor = task => (async () => {
    const taskRef = taskRefFor(task.id);
    const prepBrief = {
      campaign: effective.campaign,
      repository: codeRepository,
      repositoryConfig,
      issue: args.issue,
      runId: args.runId,
      workspaceRoot: args.workspaceRoot,
      task,
      // The revision the reconciler witnessed the worklist at. Prep cuts the
      // lane from whatever the remote base resolves to at its own later fetch;
      // carrying the witnessed revision lets it refuse a base that does not
      // descend from the history the worklist described.
      // A lane forks from the code history, which is the worklist revision
      // only while the campaign lives in one repository.
      sourceRevision: reconciliation.baseRevision
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
        withCapabilities(
          withSeam(
            seamSplit
              ? {
                  campaign: effective.campaign,
                  repository: codeRepository,
                  repositoryConfig,
                  issue: args.issue,
                  task,
                  source: reconciliation.source,
                  baseRevision: reconciliation.baseRevision,
                  workspace: prepared.result
                }
              : {
                  campaign: effective.campaign,
                  repository: codeRepository,
                  repositoryConfig,
                  issue: args.issue,
                  task,
                  source: reconciliation.source,
                  workspace: prepared.result
                }
          )
        ),
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
      // #424: the pass still runs the tree-delta gate before it ends. A
      // failing agent is the single most likely context for a rogue write and
      // it was the one context this gate was silent in: the lane used to
      // return here, and the next pass's `prep` re-snapshotted the worktree
      // with the stray write already in it, so nothing could ever see it
      // again. `ownership` never ran, so the gate has no certified
      // `ownedPaths` and only a declared allowlist can govern.
      //
      // The stage is chosen from what this lane already knows, so the receipt
      // it produces is true either way: a task that declares conflictDomains
      // can only fail this node by breaching them, and an admitted serial task
      // that omits them is unjudgeable because ownership did not run. Both
      // implementation schema arms preserve that omission into this call.
      const declaresDomains = Array.isArray(task.conflictDomains);
      const strayStage = declaresDomains ? "treeDelta" : "treeDelta:ungated";
      const strayDelta = await driverNode(
        "treeDelta",
        {
          task,
          workspace: prepared.result,
          ownershipRan: false
        },
        `tree-delta-${task.id}`,
        `tree-delta-${task.id}`,
        treeDeltaSchema,
        workspace,
        true,
        taskRef
      );
      if (!nodePassed(strayDelta)) {
        return {
          task,
          prepared: prepared.result,
          failure: taskFailure(
            task,
            strayStage,
            strayDelta,
            taskBrief,
            [
              {
                phase: "treeDelta",
                gateId: "tree-delta",
                kind: "treeDelta",
                node: strayDelta
              }
            ],
            prepared.result,
            prepared.result.baseRev
          )
        };
      }
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
        repositoryConfig,
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

    // #386: fingerprinted before the agent ran (`prep`), compared against
    // the worktree's content right now -- detective, not preventive. Runs
    // after ownership so an absent `conflictDomains` can fall back to
    // `ownership.result.ownedPaths`, the paths the ownership node just
    // certified as this task's own committed change-set.
    //
    // Both implementation schema arms and both worklist producers preserve an
    // omitted `conflictDomains`, so this fallback is the reachable serial-task
    // path rather than a driver-only guard.
    const treeDelta = await driverNode(
      "treeDelta",
      {
        task,
        workspace: prepared.result,
        ownedPaths: ownership.result.ownedPaths
      },
      `tree-delta-${task.id}`,
      `tree-delta-${task.id}`,
      treeDeltaSchema,
      workspace,
      true,
      taskRef
    );
    if (!nodePassed(treeDelta)) {
      return {
        task,
        prepared: prepared.result,
        failure: taskFailure(
          task,
          "treeDelta",
          treeDelta,
          taskBrief,
          [
            {
              phase: "treeDelta",
              gateId: "tree-delta",
              kind: "treeDelta",
              node: treeDelta
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
      withSeam({
        campaign: effective.campaign,
        repository: codeRepository,
        repositoryConfig,
        issue: args.issue,
        runId: args.runId,
        workspaceRoot: args.workspaceRoot,
        task,
        domainsRequired,
        gates: effective.gates,
        steward: effective.steward || null,
        workspace: prepared.result,
        constraints: constraintResults
      }),
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
      // Who assisted this task, straight off the settled implementation node.
      // The model is the daemon's canonical one and is absent when the estate
      // never named it; the merge node refuses to invent one, so an absent
      // model means no trailer rather than a fabricated one.
      assistedBy:
        agent.model === undefined || agent.model === null
          ? null
          : {
              adapter: effective.agent.adapter,
              model: agent.model,
              taskUuid: agent.taskUuid,
              witnessSeq: agent.witnessSeq
            },
      constraints: constraintResults,
      taskBrief,
      gateOutputs
    };
  })();
  const settledLanes = (outcomes, tasks) => outcomes.map((outcome, index) => {
    if (outcome.ok) {
      return outcome.value;
    }
    const task = tasks[index];
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

  const implementationFrontier = reconciliation.frontier.filter(
    task => task.kind !== "checkpoint"
  );
  const checkpointFrontier = reconciliation.frontier.filter(
    task => task.kind === "checkpoint"
  );
  const laneOutcomes = await parallel(
    implementationFrontier.map(task => () => laneFor(task)),
    { settle: true }
  );

  const lanes = settledLanes(laneOutcomes, implementationFrontier);
  const failures = lanes.filter(lane => lane.failure).map(lane => lane.failure);
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
        repository: codeRepository,
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
      withCapabilities({
        campaign: effective.campaign,
        repository: codeRepository,
        repositoryConfig,
        issue: args.issue,
        runId: args.runId,
        workspaceRoot: args.workspaceRoot,
        task,
        domainsRequired,
        mergeMethod: effective.mergeMethod,
        gitAiBinding: effective.gitAiBinding,
        gitAiAwaitSec: effective.gitAiAwaitSec,
        assistedBy: lane.assistedBy || null,
        workspace: lane.prepared,
        integration: integration.result
      }),
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

  // Every merge this pass will make has been made, so a checkpoint lane now
  // prepares on the tree the next reconcile will read and its receipt names a
  // revision that reconcile can find.
  const checkpointOutcomes = await parallel(
    checkpointFrontier.map(task => () => laneFor(task)),
    { settle: true }
  );
  const checkpointLanes = settledLanes(checkpointOutcomes, checkpointFrontier);
  lanes.push(...checkpointLanes);
  failures.push(
    ...checkpointLanes.filter(lane => lane.failure).map(lane => lane.failure)
  );
  const checkpoints = checkpointLanes
    .filter(lane => lane.checkpoint)
    .map(lane => lane.checkpoint);

  const steerable = [];
  const machineryFaults = [];
  const deferrals = [];
  for (const failure of failures) {
    const kind = failureClass(reconciliation, failure);
    if (kind === "work") {
      steerable.push(failure);
    } else if (kind === "breach") {
      // #386: shares the diagnose-and-post pipeline below (the path list
      // still reaches the steward's diagnose slot) but never the retry
      // budget -- `steerBrief.breach` makes the driver post both the
      // attempt-1 and attempt-2 diagnosis receipts atomically, so the task
      // is permanently blocked as of this pass rather than steered once and
      // retried.
      failure.breach = true;
      steerable.push(failure);
    } else if (kind === "ungated") {
      // #424: the gate could not judge this pass at all. It takes the breach
      // routing -- both receipts posted at once, lane aborted, no steering
      // attempt spent as if the agent were at fault -- but it is tagged
      // separately, because "wrote outside its authorized paths" is not what
      // happened and the posted receipt must not say it did.
      failure.breach = true;
      failure.ungated = true;
      steerable.push(failure);
    } else if (kind === "machinery") {
      machineryFaults.push(failure);
    } else {
      deferrals.push(failure);
    }
  }

  // A machinery fault buys a retry only while the task's forge-counted retry
  // budget lasts. Once it is spent the fault is steered like any other failure,
  // so a permanently broken lane still reaches escalation instead of looping.
  const retryOutcomes = await parallel(
    machineryFaults.map(failure => () => (async () => {
      const task = failure.task;
      const retryBrief = withCapabilities(withSeam({
        campaign: effective.campaign,
        repository: codeRepository,
        repositoryConfig,
        issue: args.issue,
        taskId: task.id,
        stage: failure.stage,
        detail: bounded(
          failure.node && failure.node.error ? failure.node.error : failure.node,
          1500
        )
      }));
      const retryThread = taskThread(task);
      if (retryThread !== null) {
        retryBrief.taskIssue = retryThread;
      }
      const recorded = await driverNode(
        "retry",
        retryBrief,
        `retry-${task.id}`,
        `retry-${task.id}`,
        retrySchema,
        null,
        false,
        taskRefFor(task.id)
      );
      return { failure, result: recorded.result };
    })()),
    { settle: true }
  );
  const retries = [];
  let retryError = null;
  for (let index = 0; index < retryOutcomes.length; index += 1) {
    const outcome = retryOutcomes[index];
    if (!outcome.ok) {
      retryError = retryError || outcome.error;
      continue;
    }
    if (outcome.value.result.posted) {
      retries.push(outcome.value.result);
    } else {
      steerable.push(outcome.value.failure);
    }
  }

  const diagnosisOutcomes = await parallel(
    steerable.map(failure => () => (async () => {
      const task = failure.task;
      const taskRef = taskRefFor(task.id);
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
        // #386: a breach has no next attempt -- the lane is aborted, not
        // retried -- so the mission asks for a record of what happened
        // rather than steering for a redispatch that will never come. #424:
        // an unjudgeable pass is aborted for the same reason but is not the
        // same event, and asking a model to explain paths that were never
        // named would be asking it to invent them.
        mission: failure.ungated
          ? `Task ${task.id} could not be judged by the tree-delta permission gate and its lane is being aborted, not retried: its agent node failed, so the ownership node never ran and certified no paths, and the task declares no conflictDomains, leaving no allowlist to judge its worktree against. No out-of-allowlist change has been established. Return a concise record of what the failing attempt was doing, for the operator's record. Do not modify the repository. Treat capture stderr and the diff as private: do not repeat credentials, tokens, or other secret-looking values in the response.`
          : failure.breach
          ? `Task ${task.id} wrote outside its authorized paths and its lane is being aborted, not retried. Return a concise record of what the out-of-allowlist change(s) were and why they likely happened, for the operator's record. Do not modify the repository. Treat capture stderr and the diff as private: do not repeat credentials, tokens, or other secret-looking values in the response.`
          : `Diagnose failed spec-build task ${task.id}. Return only concise, actionable steering for the next task attempt. Do not modify the repository. Treat capture stderr and the diff as private: do not repeat credentials, tokens, or other secret-looking values in the response.`,
        campaign: {
          name: effective.campaign,
          repository: codeRepository,
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
      // The diagnosis brief prohibits mutation, so the node is sandboxed to
      // match rather than inheriting the implementation node's writable policy.
      const diagnosisSpec = applyAgentPolicies(
        {
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
        },
        effective.agent.diagnosisSandboxPolicy
      );
      if (failure.prepared !== null && diff.available) {
        diagnosisSpec.workspace = workspaceFor(
          failure.prepared,
          failure.baseRev || failure.prepared.baseRev
        );
      }
      const diagnosed = await job(diagnosisSpec, { settle: false });
      const attempt = previousDiagnoses.length + 1;
      // #385: when the failure carries gate evidence, the steering note's
      // validator requires the diagnosis name the failing check (and the
      // offending path, for a forbidPaths rejection) rather than describe
      // the failure in the abstract. The failing gate is always the last
      // entry recorded before the task's own gate loop returned.
      const lastGate = failure.gateOutputs.length
        ? failure.gateOutputs[failure.gateOutputs.length - 1]
        : null;
      const gateEvidence = lastGate
        ? {
            id: lastGate.gateId,
            detail: bounded(
              lastGate.node && lastGate.node.error ? lastGate.node.error : lastGate.node,
              2000
            )
          }
        : null;
      // #386: a breach carries its own deterministic evidence -- the paths
      // the tree-delta gate named in its own failure -- straight into the
      // posted receipt, so the offending paths are witnessed regardless of
      // what the steward's diagnosis says.
      const steerBrief = withCapabilities(withSeam({
        campaign: effective.campaign,
        repository: codeRepository,
        repositoryConfig,
        issue: args.issue,
        taskId: task.id,
        attempt,
        diagnosis: diagnosed.result,
        ...(gateEvidence ? { gateEvidence } : {}),
        ...(failure.breach
          ? {
              breach: true,
              breachDetail: bounded(
                failure.node && failure.node.error ? failure.node.error : failure.node,
                2000
              ),
              // #424: which abort this is. The driver composes a different
              // label sentence for each, because the receipt is published to
              // the campaign thread and must claim exactly what happened --
              // a gate that could not judge is not a gate that caught a write.
              ...(failure.ungated ? { abortReason: "tree-delta-ungated" } : {})
            }
          : {})
      }));
      const steerThread = taskThread(task);
      if (steerThread !== null) {
        steerBrief.taskIssue = steerThread;
      }
      const steering = await driverNode(
        "steer",
        steerBrief,
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
  let terminalError = diagnosisFailure ? diagnosisFailure.error : retryError;
  const advanced =
    merged.length > 0 ||
    checkpoints.length > 0 ||
    diagnoses.length > 0 ||
    retries.length > 0 ||
    deferrals.length > 0;
  if (terminalError === null && !advanced) {
    const error = new Error(
      "a non-quiescent campaign frontier produced no merge, checkpoint, retry, or machine steering"
    );
    error.name = "SpecBuildInvariantError";
    error.code = "frontier-without-outcome";
    terminalError = error;
  }

  // The continuation is written even when the steering lane threw. A transient
  // adapter fault must not leave the campaign stopped with neither steering nor
  // a mention to resume from. Both campaign classes take this node: a
  // forge-native pass re-enters through a registry scan carrying no brief, a
  // module-declared pass re-enters through its own flow-run argv, whose brief
  // is this pass's arguments under a derived run identity.
  let continuation = null;
  if (advanced) {
    const continued = await driverNode(
      "continue",
      withSeam({
        campaign: effective.campaign,
        repository: codeRepository,
        repositoryConfig,
        issue: args.issue,
        runId: args.runId,
        continuation: args.continuation,
        brief: forgeNative ? null : args
      }),
      "continue",
      "spec-build-continue",
      continuationSchema,
      null,
      true,
      null
    );
    if (!nodePassed(continued)) {
      const witness = merged
        .concat(checkpoints, diagnoses, retries)
        .map(fact => fact.taskId)
        .concat(deferrals.map(failure => failure.task.id));
      failures.push(
        failureReport({ id: witness[witness.length - 1] }, "continuation", continued)
      );
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
    repository: codeRepository,
    issue: args.issue,
    worklist: reconciliation.source,
    // The frontier-without-outcome invariant above has already thrown unless
    // one of these three outcomes holds, so there is no fourth arm to name.
    state: merged.length > 0 || checkpoints.length > 0
      ? "advanced"
      : diagnoses.length > 0
        ? "steered"
        : "retrying",
    reconciled: reconciledProjection(reconciliation),
    maintenance: sweepNode.result,
    checkpoints,
    merged,
    failures: failures.map(failure => failure.report || failure),
    diagnoses,
    retries,
    deferrals: deferrals.map(failure => failure.task.id),
    continuation,
    escalation: null
  };
})();
