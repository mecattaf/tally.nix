// Generic, stateless spec-repository build reconciler.
//
// Every invocation witnesses local campaign state, selects one
// dependency-ready and conflict-disjoint frontier, advances those tasks in
// isolated worktrees, and exits. Integration commits, content-bound checkpoint
// refs, and append-only attempt receipts are the durable facts each pass folds.
export const meta = {
  name: "spec-build",
  description: "Reconcile one witnessed spec-build frontier against durable local state",
  pools: ["campaign-agent", "campaign-control"],
  // BEGIN RUST-GENERATED SPEC-BUILD ARGS SCHEMA
  argsSchema: {
    "$defs": {
      "canonicalCampaignManifest": {
        "type": "object",
        "properties": {
          "schemaVersion": {
            "type": "integer",
            "format": "uint8",
            "minimum": 0,
            "maximum": 255,
            "const": 1
          },
          "name": {
            "type": "string",
            "maxLength": 80,
            "pattern": "^[A-Za-z0-9_][A-Za-z0-9_.-]*$"
          },
          "repository": {
            "type": "object",
            "properties": {
              "checkout": {
                "type": "string",
                "pattern": "^/"
              },
              "baseBranch": {
                "type": "string",
                "minLength": 1
              },
              "remote": {
                "type": "string",
                "maxLength": 80,
                "pattern": "^[A-Za-z0-9_][A-Za-z0-9_.-]*$"
              },
              "forge": {
                "type": "string",
                "enum": [
                  "local"
                ]
              }
            },
            "additionalProperties": false,
            "required": [
              "checkout",
              "baseBranch",
              "remote",
              "forge"
            ]
          },
          "maxTasks": {
            "type": "integer",
            "format": "uint64",
            "minimum": 1,
            "maximum": 128
          },
          "maxParallel": {
            "type": "integer",
            "format": "uint64",
            "minimum": 1,
            "maximum": 128
          },
          "driverRuntimeMaxSec": {
            "type": "integer",
            "format": "uint64",
            "minimum": 1
          },
          "runtimeMaxSec": {
            "type": [
              "integer",
              "null"
            ],
            "format": "uint64",
            "minimum": 1
          },
          "pool": {
            "type": "string",
            "maxLength": 80,
            "pattern": "^(?:[A-Za-z0-9_][A-Za-z0-9_.-]*|campaign/(?!\\.{1,2}/)[A-Za-z0-9_.-]+/(?!\\.{1,2}$)[A-Za-z0-9_.-]+)$"
          },
          "mergeMethod": {
            "type": "string",
            "enum": [
              "merge",
              "squash"
            ]
          },
          "agent": {
            "$ref": "#/$defs/canonicalAgent"
          },
          "steward": {
            "anyOf": [
              {
                "$ref": "#/$defs/canonicalSteward"
              },
              {
                "type": "null"
              }
            ]
          },
          "gates": {
            "type": "array",
            "items": {
              "$ref": "#/$defs/canonicalGate"
            },
            "minItems": 1,
            "maxItems": 16
          },
          "tasks": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "id": {
                  "type": "string"
                },
                "kind": {
                  "type": "string",
                  "enum": [
                    "implementation",
                    "checkpoint"
                  ]
                },
                "issue": {
                  "type": "integer",
                  "format": "int64"
                },
                "dependencies": {
                  "type": "array",
                  "items": true
                },
                "conflictDomains": {
                  "type": "array",
                  "items": true
                },
                "argv": {
                  "type": [
                    "array",
                    "null"
                  ],
                  "items": true
                },
                "runtimeMaxSec": {
                  "type": [
                    "integer",
                    "null"
                  ],
                  "format": "int64"
                }
              },
              "additionalProperties": false,
              "required": [
                "id",
                "kind",
                "issue",
                "dependencies",
                "argv",
                "runtimeMaxSec"
              ]
            },
            "minItems": 1,
            "maxItems": 128
          }
        },
        "additionalProperties": false,
        "required": [
          "schemaVersion",
          "name",
          "repository",
          "maxTasks",
          "maxParallel",
          "driverRuntimeMaxSec",
          "runtimeMaxSec",
          "pool",
          "mergeMethod",
          "agent",
          "steward",
          "gates",
          "tasks"
        ]
      },
      "canonicalAgent": {
        "type": "object",
        "properties": {
          "adapter": {
            "type": "string",
            "minLength": 1
          },
          "argv": {
            "$ref": "#/$defs/canonicalArgv"
          },
          "priority": {
            "type": "string",
            "enum": [
              "interrupt",
              "high",
              "medium",
              "low"
            ]
          },
          "runtimeMaxSec": {
            "type": [
              "integer",
              "null"
            ],
            "format": "uint64",
            "minimum": 1
          },
          "approvalPolicy": {
            "type": [
              "string",
              "null"
            ],
            "minLength": 1
          },
          "sandboxPolicy": {
            "type": [
              "string",
              "null"
            ],
            "minLength": 1
          },
          "diagnosisSandboxPolicy": {
            "type": [
              "string",
              "null"
            ],
            "minLength": 1
          },
          "model": {
            "type": [
              "string",
              "null"
            ],
            "minLength": 1,
            "maxLength": 128
          }
        },
        "additionalProperties": false,
        "required": [
          "adapter",
          "argv",
          "priority",
          "runtimeMaxSec",
          "approvalPolicy",
          "sandboxPolicy",
          "diagnosisSandboxPolicy",
          "model"
        ]
      },
      "canonicalArgv": {
        "type": "array",
        "items": {
          "type": "string",
          "minLength": 1,
          "pattern": "^[^\\u0000-\\u001f\\u007f]+$"
        },
        "minItems": 1
      },
      "canonicalSteward": {
        "type": "object",
        "properties": {
          "adapter": {
            "type": "string",
            "minLength": 1,
            "maxLength": 80,
            "pattern": "^[A-Za-z0-9_][A-Za-z0-9_.-]*$"
          },
          "argv": {
            "$ref": "#/$defs/canonicalArgv"
          },
          "env": {
            "type": "object",
            "additionalProperties": {
              "type": "string",
              "minLength": 1,
              "maxLength": 4096
            },
            "maxProperties": 64,
            "propertyNames": {
              "pattern": "^[A-Za-z_][A-Za-z0-9_]*$"
            }
          },
          "finalMessagePattern": {
            "type": "string",
            "minLength": 1,
            "maxLength": 1024
          },
          "runtimeMaxSec": {
            "type": [
              "integer",
              "null"
            ],
            "format": "uint64",
            "minimum": 1
          }
        },
        "additionalProperties": false,
        "required": [
          "adapter",
          "argv",
          "env",
          "finalMessagePattern",
          "runtimeMaxSec"
        ]
      },
      "canonicalGate": {
        "oneOf": [
          {
            "type": "object",
            "properties": {
              "id": {
                "type": "string",
                "maxLength": 80,
                "pattern": "^[A-Za-z0-9_][A-Za-z0-9_.-]*$"
              },
              "preflightArgv": {
                "$ref": "#/$defs/canonicalArgv"
              },
              "argv": {
                "$ref": "#/$defs/canonicalArgv"
              },
              "runtimeMaxSec": {
                "type": "integer",
                "format": "uint64",
                "minimum": 1
              },
              "kind": {
                "type": "string",
                "const": "command"
              }
            },
            "additionalProperties": false,
            "required": [
              "kind",
              "id",
              "preflightArgv",
              "argv",
              "runtimeMaxSec"
            ]
          },
          {
            "type": "object",
            "properties": {
              "id": {
                "type": "string",
                "maxLength": 80,
                "pattern": "^[A-Za-z0-9_][A-Za-z0-9_.-]*$"
              },
              "forbidPaths": {
                "type": "array",
                "items": {
                  "type": "string",
                  "minLength": 1,
                  "maxLength": 1024
                },
                "minItems": 1,
                "maxItems": 128,
                "uniqueItems": true
              },
              "runtimeMaxSec": {
                "type": "integer",
                "format": "uint64",
                "minimum": 1
              },
              "kind": {
                "type": "string",
                "const": "forbidPaths"
              }
            },
            "additionalProperties": false,
            "required": [
              "kind",
              "id",
              "forbidPaths",
              "runtimeMaxSec"
            ]
          }
        ]
      }
    },
    "allOf": [
      {
        "if": {
          "required": [
            "taskSteering"
          ]
        },
        "then": {
          "required": [
            "localActor",
            "steeringSource"
          ]
        }
      },
      {
        "if": {
          "required": [
            "localActor"
          ]
        },
        "then": {
          "required": [
            "steeringSource"
          ]
        }
      },
      {
        "if": {
          "required": [
            "steeringSource"
          ]
        },
        "then": {
          "required": [
            "localActor"
          ]
        }
      }
    ],
    "type": "object",
    "properties": {
      "campaign": {
        "type": "string",
        "maxLength": 80,
        "pattern": "^[A-Za-z0-9_][A-Za-z0-9_.-]*$"
      },
      "campaignIdentity": {
        "type": "string",
        "pattern": "^[0-9a-fA-F-]{36}$"
      },
      "campaignGraph": {
        "type": "object",
        "properties": {
          "manifest": {
            "$ref": "#/$defs/canonicalCampaignManifest"
          },
          "tasks": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "number": {
                  "type": "integer",
                  "format": "uint64",
                  "minimum": 1
                },
                "title": {
                  "type": "string",
                  "minLength": 1,
                  "maxLength": 300
                },
                "body": {
                  "type": "string",
                  "minLength": 1,
                  "maxLength": 64000
                }
              },
              "additionalProperties": false,
              "required": [
                "number",
                "title",
                "body"
              ]
            },
            "minItems": 1,
            "maxItems": 128
          },
          "executableDigest": {
            "type": "string",
            "pattern": "^sha256:[0-9a-f]{64}$"
          }
        },
        "additionalProperties": false,
        "required": [
          "manifest",
          "tasks",
          "executableDigest"
        ]
      },
      "armedManifest": {
        "anyOf": [
          {
            "$ref": "#/$defs/canonicalCampaignManifest"
          },
          {
            "type": "null"
          }
        ]
      },
      "allowedActors": {
        "type": "array",
        "items": true
      },
      "capabilities": {
        "type": "object",
        "additionalProperties": true
      },
      "repository": {
        "type": "string",
        "pattern": "^[^/ \\t]+/[^/ \\t]+$"
      },
      "codeRepository": {
        "type": "string",
        "pattern": "^[^/ \\t]+/[^/ \\t]+$"
      },
      "specRepository": {
        "type": "string",
        "pattern": "^[^/ \\t]+/[^/ \\t]+$"
      },
      "issueRepository": {
        "type": "string",
        "pattern": "^[^/ \\t]+/[^/ \\t]+$"
      },
      "issue": {
        "type": "object",
        "properties": {
          "number": {
            "type": "string",
            "pattern": "^[1-9][0-9]*$"
          },
          "url": {
            "type": "string",
            "minLength": 1
          }
        },
        "additionalProperties": false,
        "required": [
          "number",
          "url"
        ]
      },
      "runId": {
        "type": "string",
        "minLength": 1,
        "maxLength": 512
      },
      "repositories": {
        "type": "object",
        "additionalProperties": {
          "type": "object",
          "properties": {
            "checkout": {
              "type": "string",
              "pattern": "^/"
            },
            "baseBranch": {
              "type": "string",
              "pattern": "^[A-Za-z0-9._/+-]+$"
            },
            "remote": {
              "type": "string",
              "pattern": "^[A-Za-z0-9._-]+$"
            },
            "forge": {
              "type": "string",
              "enum": [
                "github",
                "local"
              ]
            }
          },
          "additionalProperties": false,
          "required": [
            "checkout",
            "baseBranch",
            "remote",
            "forge"
          ]
        },
        "minProperties": 1
      },
      "worklist": {
        "anyOf": [
          {
            "type": "string",
            "minLength": 1
          },
          {
            "type": "object",
            "properties": {
              "kind": {
                "type": "string",
                "minLength": 1
              },
              "graphDigest": {
                "type": "string",
                "pattern": "^sha256:[0-9a-f]{64}$"
              }
            },
            "additionalProperties": false,
            "required": [
              "kind",
              "graphDigest"
            ]
          }
        ]
      },
      "maxTasks": {
        "type": "integer",
        "format": "uint64",
        "minimum": 1,
        "maximum": 128
      },
      "maxParallel": {
        "type": "integer",
        "format": "uint64",
        "minimum": 1,
        "maximum": 128
      },
      "continuation": {
        "type": "object",
        "properties": {
          "argv": {
            "type": "array",
            "items": {
              "type": "string",
              "minLength": 1,
              "pattern": "^[^\\u0000-\\u001f\\u007f]+$"
            },
            "minItems": 1,
            "maxItems": 64
          },
          "pool": {
            "type": "array",
            "items": {
              "type": "string",
              "minLength": 1,
              "maxLength": 128
            },
            "minItems": 1,
            "maxItems": 8,
            "uniqueItems": true
          },
          "priority": {
            "type": "string",
            "enum": [
              "interrupt",
              "high",
              "medium",
              "low"
            ]
          },
          "runtimeMaxSec": {
            "type": [
              "integer",
              "null"
            ],
            "format": "uint64",
            "minimum": 1
          },
          "eventsDir": {
            "type": "string",
            "pattern": "^/"
          }
        },
        "additionalProperties": false,
        "required": [
          "argv",
          "pool",
          "priority",
          "eventsDir"
        ]
      },
      "workspaceRoot": {
        "type": "string",
        "pattern": "^/"
      },
      "captureRoot": {
        "type": "string",
        "pattern": "^/.*/capture/archive$"
      },
      "tally": {
        "type": "string",
        "pattern": "^/"
      },
      "driver": {
        "type": "string",
        "pattern": "^/"
      },
      "driverRuntimeMaxSec": {
        "type": "integer",
        "format": "uint64",
        "minimum": 1
      },
      "postFailureEvidence": {
        "type": "boolean"
      },
      "postFailureStderr": {
        "type": "boolean"
      },
      "steering": {
        "type": "array",
        "items": {
          "type": "object",
          "properties": {
            "id": {
              "type": "integer",
              "format": "uint64",
              "minimum": 1
            },
            "url": {
              "type": "string",
              "minLength": 1
            },
            "author": {
              "type": "string",
              "minLength": 1,
              "maxLength": 128
            },
            "body": {
              "type": "string",
              "maxLength": 64000
            },
            "createdAt": {
              "type": "string",
              "minLength": 1
            },
            "updatedAt": {
              "type": "string",
              "minLength": 1
            }
          },
          "additionalProperties": false,
          "required": [
            "id",
            "url",
            "author",
            "body",
            "createdAt",
            "updatedAt"
          ]
        },
        "maxItems": 1000
      },
      "localActor": {
        "type": "string",
        "minLength": 1,
        "maxLength": 128,
        "pattern": "^[^\\s/\\\\\\u0000]+$"
      },
      "steeringSource": {
        "type": "object",
        "properties": {
          "schemaVersion": {
            "type": "integer",
            "format": "uint8",
            "minimum": 0,
            "maximum": 255,
            "const": 1
          },
          "kind": {
            "type": "string",
            "enum": [
              "local-jsonl"
            ]
          },
          "registrationId": {
            "type": "string",
            "minLength": 1,
            "maxLength": 128
          },
          "localActor": {
            "type": "string",
            "minLength": 1,
            "maxLength": 128,
            "pattern": "^[^\\s/\\\\\\u0000]+$"
          },
          "logPath": {
            "type": "string",
            "pattern": "^/"
          },
          "lockPath": {
            "type": "string",
            "pattern": "^/"
          },
          "preparedCursor": {
            "type": "integer",
            "format": "uint64",
            "minimum": 0
          }
        },
        "additionalProperties": false,
        "required": [
          "schemaVersion",
          "kind",
          "registrationId",
          "localActor",
          "logPath",
          "lockPath",
          "preparedCursor"
        ]
      },
      "taskSteering": {
        "type": "object",
        "additionalProperties": {
          "type": "array",
          "items": {
            "type": "object",
            "properties": {
              "id": {
                "type": "integer",
                "format": "uint64",
                "minimum": 1
              },
              "url": {
                "type": "string",
                "minLength": 1
              },
              "author": {
                "type": "string",
                "minLength": 1,
                "maxLength": 128
              },
              "body": {
                "type": "string",
                "maxLength": 64000
              },
              "createdAt": {
                "type": "string",
                "minLength": 1
              },
              "updatedAt": {
                "type": "string",
                "minLength": 1
              }
            },
            "additionalProperties": false,
            "required": [
              "id",
              "url",
              "author",
              "body",
              "createdAt",
              "updatedAt"
            ]
          },
          "maxItems": 1000
        },
        "maxProperties": 128
      },
      "mergeMethod": {
        "type": "string",
        "enum": [
          "merge",
          "squash"
        ]
      },
      "steward": {
        "anyOf": [
          {
            "$ref": "#/$defs/canonicalSteward"
          },
          {
            "type": "null"
          }
        ]
      },
      "agent": {
        "type": "object",
        "properties": {
          "adapter": {
            "type": "string",
            "minLength": 1
          },
          "argv": {
            "$ref": "#/$defs/canonicalArgv"
          },
          "priority": {
            "type": "string",
            "enum": [
              "interrupt",
              "high",
              "medium",
              "low"
            ]
          },
          "runtimeMaxSec": {
            "type": [
              "integer",
              "null"
            ],
            "format": "uint64",
            "minimum": 1
          },
          "approvalPolicy": {
            "type": [
              "string",
              "null"
            ],
            "minLength": 1
          },
          "sandboxPolicy": {
            "type": [
              "string",
              "null"
            ],
            "minLength": 1
          },
          "diagnosisSandboxPolicy": {
            "type": [
              "string",
              "null"
            ],
            "minLength": 1
          },
          "model": {
            "type": [
              "string",
              "null"
            ],
            "minLength": 1,
            "maxLength": 128
          }
        },
        "additionalProperties": false,
        "required": [
          "adapter",
          "argv",
          "priority",
          "runtimeMaxSec",
          "approvalPolicy",
          "sandboxPolicy",
          "diagnosisSandboxPolicy",
          "model"
        ]
      },
      "gates": {
        "type": "array",
        "items": {
          "$ref": "#/$defs/canonicalGate"
        },
        "minItems": 1,
        "maxItems": 16,
        "uniqueItems": true
      }
    },
    "additionalProperties": false,
    "required": [
      "repository",
      "issue",
      "runId",
      "worklist",
      "continuation",
      "workspaceRoot",
      "captureRoot",
      "tally",
      "driver",
      "driverRuntimeMaxSec"
    ],
    "oneOf": [
      {
        "properties": {
          "worklist": {
            "type": "string",
            "minLength": 1
          }
        },
        "required": [
          "campaign",
          "repositories",
          "maxTasks",
          "maxParallel",
          "agent",
          "gates"
        ]
      },
      {
        "properties": {
          "worklist": {
            "type": "object"
          }
        },
        "required": [
          "campaignIdentity",
          "campaignGraph",
          "steering",
          "localActor",
          "steeringSource"
        ]
      }
    ]
  },
  // END RUST-GENERATED SPEC-BUILD ARGS SCHEMA
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
    "dependencies",
    "revision"
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
    conflictDomains: stringList,
    revision: { type: "string", pattern: "^sha256:[0-9a-f]{64}$" }
  },
  additionalProperties: false
};

const checkpointTaskSchema = {
  type: "object",
  required: ["id", "kind", "title", "argv", "runtimeMaxSec", "dependencies", "revision"],
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
    },
    revision: { type: "string", pattern: "^sha256:[0-9a-f]{64}$" }
  },
  additionalProperties: false
};

const taskSchema = {
  oneOf: [implementationTaskSchema, checkpointTaskSchema]
};

const sourceSchema = {
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

const diagnosisProposalSchema = {
  type: "object",
  required: ["kind", "paths", "goal", "acceptanceCriteria", "dependencies"],
  properties: {
    kind: { enum: ["amendment-task", "gate-set-fix"] },
    paths: {
      type: "array",
      minItems: 1,
      maxItems: 128,
      uniqueItems: true,
      items: { type: "string", minLength: 1, maxLength: 4096 }
    },
    goal: { type: "string", minLength: 1, maxLength: 12000 },
    acceptanceCriteria: {
      type: "array",
      minItems: 1,
      maxItems: 16,
      items: {
        type: "object",
        required: ["id", "description", "argv"],
        properties: {
          id: {
            type: "string",
            maxLength: 80,
            pattern: "^[A-Za-z0-9_][A-Za-z0-9_.-]*$"
          },
          description: { type: "string", minLength: 1, maxLength: 4000 },
          argv: {
            type: "array",
            minItems: 1,
            maxItems: 32,
            items: { type: "string", minLength: 1, maxLength: 4096 }
          }
        },
        additionalProperties: false
      }
    },
    dependencies: {
      type: "array",
      maxItems: 128,
      uniqueItems: true,
      items: taskIdSchema
    }
  },
  additionalProperties: false
};

const diagnosisFactSchema = {
  type: "object",
  required: ["taskId", "attempt", "comment", "diagnosis", "verdict"],
  properties: {
    taskId: taskIdSchema,
    attempt: { type: "integer", minimum: 1, maximum: 2 },
    comment: { type: "string", minLength: 1 },
    diagnosis: { type: "string", minLength: 1, maxLength: 12000 },
    verdict: { enum: ["retry", "blocked", "transient"] },
    proposal: diagnosisProposalSchema
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

const workerOutcomeFactSchema = {
  type: "object",
  required: [
    "taskId",
    "taskRevision",
    "taskUuid",
    "outcome",
    "comment",
    "paths",
    "reason"
  ],
  properties: {
    taskId: taskIdSchema,
    taskRevision: { type: "string", pattern: "^sha256:[0-9a-f]{64}$" },
    taskUuid: { type: "string", minLength: 36, maxLength: 36 },
    outcome: { enum: ["needs-authority", "impossible"] },
    comment: { type: "string", minLength: 1 },
    paths: {
      type: ["array", "null"],
      minItems: 1,
      maxItems: 128,
      uniqueItems: true,
      items: { type: "string", minLength: 1, maxLength: 4096 }
    },
    reason: { type: ["string", "null"], minLength: 1, maxLength: 12000 }
  },
  additionalProperties: false
};

const workerOutcomeRecordSchema = {
  type: "object",
  required: [
    "taskId",
    "taskRevision",
    "taskUuid",
    "outcome",
    "comment",
    "paths",
    "reason",
    "recorded",
    "attemptCost"
  ],
  properties: {
    ...workerOutcomeFactSchema.properties,
    recorded: { type: "boolean" },
    attemptCost: { const: 0 }
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
    outcomes: { type: "array", maxItems: 256, items: workerOutcomeFactSchema },
    deferrals: { type: "array", maxItems: 128, items: deferralFactSchema },
    blocked: { type: "array", maxItems: 128, items: blockedFactSchema },
    quiescent: { type: "boolean" },
    escalation: { type: ["string", "null"], minLength: 1 },
    complete: { type: "boolean" },
    warnings: stringList,
    // Where the completion path published this campaign's closing summary, or
    // null on any pass that was not the terminal one.
    closingSummary: { type: ["string", "null"], minLength: 1 }
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
    worktreePath: { type: "string", pattern: "^/" },
    conflictDomains: stringList
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
  required: [
    "taskId",
    "passed",
    "ref",
    "revision",
    "capturePath",
    "stdoutTruncated",
    "stderrTruncated"
  ],
  properties: {
    taskId: taskIdSchema,
    passed: { type: "boolean" },
    // New receipts land in the hidden state namespace; already-published
    // visible tag receipts stay honored.
    ref: { type: ["string", "null"], pattern: "^refs/(tags/)?tally/spec-build/v1/" },
    revision: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    capturePath: { type: "string", pattern: "^/" },
    stdoutTruncated: { type: "boolean" },
    stderrTruncated: { type: "boolean" }
  },
  additionalProperties: false
};

// The validated text the integration commit will carry. `source` records
// whether the steward authored it or the deterministic template did.
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
// publish node's result and never leaves local campaign state.
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

const diagnosisBriefSentinel =
  "Read the file whose path is in the TALLY_BRIEF environment variable and execute the mission it contains. That brief is your complete instruction set.";

// Semantic identity is separate from the operator-facing label. NodeSpec does
// not expose arbitrary orchestration fields, so the explicit flow-local key
// carries this versioned closed schema to tally-core admission; core persists
// it as orchestration.nodeRole + subjectTaskId before any campaign fold reads
// the node. Keep this JSON object literal parseable: a tally-core parity test
// pins its values against the Rust enum.
const specBuildNodeRole = Object.freeze({
  "AGENT": "agent",
  "CHECKPOINT_RECORD": "checkpoint-record",
  "CLEANUP": "cleanup",
  "CONSTRAINT": "constraint",
  "CONTINUE": "continue",
  "DIAGNOSIS": "diagnosis",
  "ESCALATE": "escalate",
  "GATE": "gate",
  "MERGE": "merge",
  "OWNERSHIP": "ownership",
  "PREP": "prep",
  "PUBLISH": "publish",
  "REBASE": "rebase",
  "RECONCILE": "reconcile",
  "RETRY": "retry",
  "STEERING": "steering",
  "SWEEP": "sweep"
});

const specBuildNodeRoleSchema = Object.freeze({
  type: "string",
  enum: Object.freeze(Object.values(specBuildNodeRole))
});

const driverActionNodeRole = Object.freeze({
  sweep: specBuildNodeRole.SWEEP,
  reconcile: specBuildNodeRole.RECONCILE,
  diff: specBuildNodeRole.DIAGNOSIS,
  outcome: specBuildNodeRole.STEERING,
  steeringRecheck: specBuildNodeRole.STEERING,
  steer: specBuildNodeRole.STEERING,
  retry: specBuildNodeRole.RETRY,
  escalate: specBuildNodeRole.ESCALATE,
  continue: specBuildNodeRole.CONTINUE,
  preflight: specBuildNodeRole.PREP,
  prep: specBuildNodeRole.PREP,
  cleanup: specBuildNodeRole.CLEANUP,
  ownership: specBuildNodeRole.OWNERSHIP,
  treeDelta: specBuildNodeRole.CONSTRAINT,
  constraint: specBuildNodeRole.CONSTRAINT,
  checkpoint: specBuildNodeRole.CHECKPOINT_RECORD,
  publish: specBuildNodeRole.PUBLISH,
  rebase: specBuildNodeRole.REBASE,
  merge: specBuildNodeRole.MERGE
});

function specBuildNodeIdentity(role, taskRef, key, label) {
  if (!specBuildNodeRoleSchema.enum.includes(role)) {
    throw new TypeError(`unknown spec-build node role ${String(role)}`);
  }
  if (typeof key !== "string" || key.length === 0 || key.includes(":")) {
    throw new TypeError("spec-build node key must be a non-empty colon-free string");
  }
  if (typeof label !== "string" || label.length === 0) {
    throw new TypeError("spec-build node label must be a non-empty string");
  }
  let subjectTaskId = "";
  if (taskRef !== null) {
    if (typeof taskRef !== "string") {
      throw new TypeError("spec-build node taskRef must be a string or null");
    }
    const components = taskRef.split("/");
    if (components.length !== 2 || components.some(component => component.length === 0)) {
      throw new TypeError("spec-build node taskRef must contain one non-empty task component");
    }
    subjectTaskId = components[1];
  }
  return {
    key: `spec-build:v1:${role}:${subjectTaskId}:${key}`,
    label
  };
}

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

const mergeSchema = {
  type: "object",
  required: [
    "taskId",
    "head",
    "mergeCommit",
    "pullRequest",
    "regated",
    "ownership",
    "trailer"
  ],
  properties: {
    taskId: taskIdSchema,
    head: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    mergeCommit: { type: "string", pattern: "^[0-9a-f]{40,64}$" },
    pullRequest: { type: "string", minLength: 1 },
    regated: { type: "boolean" },
    ownership: ownershipSchema,
    // The exact `Assisted-by:` line the node wrote into the squash message,
    // or null when the campaign could not name the assisting session. The
    // trailer is a pointer into the witness ledger.
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

const steeringSchema = {
  type: "object",
  required: ["kind", "taskId", "attempt", "comment", "blocked", "posted", "redacted"],
  properties: {
    kind: { enum: ["diagnosis", "retry"] },
    taskId: taskIdSchema,
    attempt: { type: "integer", minimum: 1, maximum: 2 },
    comment: { type: "string", minLength: 1 },
    verdict: { enum: ["retry", "blocked", "transient"] },
    proposal: diagnosisProposalSchema,
    blocked: { type: "boolean" },
    posted: { type: "boolean" },
    redacted: { type: "boolean" },
    exhausted: { type: "boolean" },
    retry: {
      anyOf: [retrySchema, { type: "null" }]
    }
  },
  additionalProperties: false
};

const authorizedSteeringCommentSchema = {
  type: "object",
  required: ["id", "url", "author", "body", "createdAt", "updatedAt"],
  properties: {
    id: { type: "integer", minimum: 1 },
    url: { type: "string", minLength: 1 },
    author: { type: "string", minLength: 1, maxLength: 128 },
    body: { type: "string", maxLength: 64000 },
    createdAt: { type: "string", minLength: 1 },
    updatedAt: { type: "string", minLength: 1 }
  },
  additionalProperties: false
};

const steeringRecheckSchema = {
  type: "object",
  required: ["taskId", "authorizedComments", "receipt"],
  properties: {
    taskId: taskIdSchema,
    authorizedComments: {
      type: "array",
      maxItems: 2000,
      items: authorizedSteeringCommentSchema
    },
    receipt: {
      type: "object",
      required: [
        "source",
        "rechecked",
        "recheckTruncated",
        "preparedCommentIds",
        "lateRecheckCommentIds"
      ],
      properties: {
        source: {
          type: "object",
          required: [
            "kind",
            "registrationId",
            "path",
            "preparedCursor",
            "recheckedCursor"
          ],
          properties: {
            kind: { const: "local-jsonl" },
            registrationId: { type: "string", minLength: 1, maxLength: 128 },
            path: { type: "string", pattern: "^/" },
            preparedCursor: { type: "integer", minimum: 0 },
            recheckedCursor: { type: "integer", minimum: 0 }
          },
          additionalProperties: false
        },
        rechecked: { const: true },
        recheckTruncated: { type: "boolean" },
        preparedCommentIds: {
          type: "array",
          maxItems: 2000,
          uniqueItems: true,
          items: { type: "integer", minimum: 1 }
        },
        lateRecheckCommentIds: {
          type: "array",
          maxItems: 1000,
          uniqueItems: true,
          items: { type: "integer", minimum: 1 }
        }
      },
      additionalProperties: false
    }
  },
  additionalProperties: false
};

const escalationSchema = {
  type: "object",
  required: ["posted", "comment", "summary", "diagnosisCount", "retryCount"],
  properties: {
    posted: { type: "boolean" },
    comment: { type: "string", minLength: 1 },
    // The closing summary recorded beside the escalation, reflecting partial
    // state. Null only when this pass found an escalation already recorded.
    summary: { type: ["string", "null"], minLength: 1 },
    // A needs-authority receipt can make the frontier quiescent without
    // spending a diagnosis attempt.
    diagnosisCount: { type: "integer", minimum: 0, maximum: 256 },
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

const diagnosisResultSchema = {
  type: "object",
  required: ["verdict", "diagnosis"],
  properties: {
    verdict: { enum: ["retry", "blocked", "transient"] },
    diagnosis: { type: "string", minLength: 1, maxLength: 12000 },
    proposal: diagnosisProposalSchema
  },
  additionalProperties: false,
  allOf: [
    {
      if: {
        required: ["verdict"],
        properties: { verdict: { const: "blocked" } }
      },
      else: { not: { required: ["proposal"] } }
    }
  ]
};

const legacyDiagnosisResultSchema = {
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
  const identity = specBuildNodeIdentity(
    driverActionNodeRole[action],
    taskRef,
    key,
    label
  );
  const spec = {
    argv: [args.driver, action],
    adapter: "spec-build-driver",
    pools: ["campaign-control"],
    priority: "low",
    runtimeMaxSec,
    evidence: ["exit:0"],
    brief,
    key: identity.key,
    label: identity.label
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
let attemptReceipts = null;

// The campaign's three repository coordinates, resolved once. `code` is where
// lanes, stable task branches and the local integration branch live; `spec` is
// where the worklist artifact is read from; `issue` is where the campaign
// thread and every machine receipt live. Each defaults inward -- issue to spec,
// spec and code to the repository the campaign issue was read from -- so a
// campaign that names none of them resolves all three to the same repository
// and takes exactly the pre-seam path.
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

// Normalize both admitted entry points onto the local file-worklist contract.
// Direct module invocations already carry that contract. campaign.rs currently
// carries the admitted manifest beside an opaque selector, while its local
// issue URL retains the committed worklist pattern. The issue task projection
// is intentionally ignored: the driver reads and validates the pinned file.
function campaignInputs() {
  if (args.campaignGraph === undefined) {
    return {
      campaign: args.campaign,
      repositoryConfig: args.repositories[codeRepository],
      worklist: args.worklist,
      maxTasks: args.maxTasks,
      maxParallel: args.maxParallel,
      mergeMethod: args.mergeMethod || "squash",
      postFailureEvidence: args.postFailureEvidence === true,
      postFailureStderr: args.postFailureStderr === true,
      agent: diagnosisSandboxed(args.agent),
      steward: args.steward || null,
      gates: args.gates
    };
  }

  const graph = args.campaignGraph;
  if (args.worklist.graphDigest !== graph.executableDigest) {
    const error = new Error(
      "campaign selector digest does not match the admitted local campaign graph"
    );
    error.name = "SpecBuildConfigurationError";
    error.code = "campaign-graph-digest-mismatch";
    throw error;
  }
  const localPrefix = `local://${args.repository}/`;
  if (!args.issue.url.startsWith(localPrefix)) {
    const error = new Error(
      `campaign issue URL must carry the local worklist pattern under ${localPrefix}`
    );
    error.name = "SpecBuildConfigurationError";
    error.code = "local-worklist-url-invalid";
    throw error;
  }
  const worklist = args.issue.url.slice(localPrefix.length);
  if (worklist.length === 0) {
    const error = new Error("campaign issue URL carries an empty local worklist pattern");
    error.name = "SpecBuildConfigurationError";
    error.code = "local-worklist-url-invalid";
    throw error;
  }
  const manifest = graph.manifest;
  return {
    campaign: manifest.name,
    repositoryConfig: manifest.repository,
    worklist,
    maxTasks: manifest.maxTasks,
    maxParallel: manifest.maxParallel,
    mergeMethod: manifest.mergeMethod,
    postFailureEvidence: false,
    postFailureStderr: false,
    agent: diagnosisSandboxed(manifest.agent),
    steward: manifest.steward,
    gates: manifest.gates
  };
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

const FORBID_PATHS_FAILURE =
  /forbidPaths gate \S+ rejected \d+ path\(s\) touched in lane history \(a later removal does not clear this; the path must never appear in any lane commit\): "((?:[^"\\]|\\.)*)"/;

function gateEvidenceForFailure(failure) {
  const lastGate = failure.gateOutputs.length
    ? failure.gateOutputs[failure.gateOutputs.length - 1]
    : null;
  if (lastGate === null) {
    return null;
  }
  const node = lastGate.node;
  let detail = node;
  if (node && node.stderrExcerpt) {
    detail = node.stderrExcerpt;
  } else if (node && node.error) {
    detail = node.error;
  }
  return {
    id: lastGate.gateId,
    // A non-zero driver exit has no result to project, so its generic node
    // error can obscure the stderr detail that names the forbidPaths breach.
    // Prefer the captured gate output: the diagnosis prompt and validator must
    // derive their literal path from the same failure the operator sees.
    detail: bounded(detail, 2000)
  };
}

function diagnosisLiteralSubstringRule(gateEvidence) {
  const rule =
    " Public diagnosis grammar requirement: a diagnosis for a failing gate " +
    "MUST contain the failing check id and, when named by the gate, the " +
    "offending path as literal substrings; paraphrases do not count.";
  if (gateEvidence === null) {
    return `${rule} This failure has no failing-gate literal to copy.`;
  }
  const matched = FORBID_PATHS_FAILURE.exec(gateEvidence.detail);
  if (matched === null) {
    return (
      `${rule} For this failure, copy the exact failing check id ` +
      `${JSON.stringify(gateEvidence.id)} unchanged into the diagnosis.`
    );
  }
  return (
    `${rule} For this failure, copy both exact strings unchanged into the ` +
    `diagnosis: failing check id ${JSON.stringify(gateEvidence.id)}; ` +
    `offending path ${JSON.stringify(matched[1])}.`
  );
}

function forbidPathsHistoryRule(gateEvidence) {
  if (
    gateEvidence === null ||
    FORBID_PATHS_FAILURE.exec(gateEvidence.detail) === null
  ) {
    return "";
  }
  return (
    " forbidPaths history rule: the gate walks " +
    "`changed_paths_in_history(union_base, head)`, so a later removal does not " +
    "clear an earlier commit. The only cure is rewriting the lane so the path " +
    "never appears in any commit: soft-reset to the merge base and recommit " +
    "without it."
  );
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

function machineOutcomes(reconciliation, taskId) {
  return reconciliation.outcomes.filter(item => item.taskId === taskId);
}

// Only the two exact final-message shapes are envelopes. Anything else stays
// on the existing signal path: a non-zero agent exit is still an agent failure,
// and a zero exit with no commit still reaches ownership unchanged.
function workerOutcomeEnvelope(result) {
  if (result === null || typeof result !== "object" || Array.isArray(result)) {
    return null;
  }
  const fields = Object.keys(result).sort();
  if (
    result.outcome === "needs-authority" &&
    JSON.stringify(fields) === JSON.stringify(["outcome", "paths"]) &&
    Array.isArray(result.paths) &&
    result.paths.length > 0 &&
    result.paths.length <= 128 &&
    result.paths.every(path => {
      if (typeof path !== "string" || path.length === 0 || path.length > 4096) {
        return false;
      }
      const components = path.split("/");
      return (
        !path.startsWith("/") &&
        !path.endsWith("/") &&
        !components.some(component => component === "" || component === "." || component === "..") &&
        !Array.from(path).some(character => character.codePointAt(0) < 32)
      );
    }) &&
    new Set(result.paths).size === result.paths.length
  ) {
    return result;
  }
  if (
    result.outcome === "impossible" &&
    JSON.stringify(fields) === JSON.stringify(["outcome", "reason"]) &&
    typeof result.reason === "string" &&
    result.reason.trim().length > 0 &&
    Array.from(result.reason).length <= 12000 &&
    !Array.from(result.reason).some(character => {
      const code = character.codePointAt(0);
      return code < 32 && ![9, 10, 13].includes(code);
    })
  ) {
    return result;
  }
  return null;
}

// A campaign has two unrelated kinds of failure and must not price them the
// same. A red gate, a rejected ownership boundary, an agent that exits non-zero
// and a red checkpoint command are all evidence that the task's work is wrong,
// and each one spends one of the task's two steering attempts. Preparing a
// worktree, rebasing, publishing and merging are campaign machinery: when they
// fault they say nothing about the work, so they buy a bounded receipt-counted
// retry instead. A checkpoint that runs while unrelated implementation work is
// still outstanding is neither - its verdict is not yet meaningful.
function failureClass(reconciliation, failure) {
  const stage = failure.stage;
  if (failure.outcome && failure.outcome.outcome === "needs-authority") {
    return "needs-authority";
  }
  if (failure.outcome && failure.outcome.outcome === "impossible") {
    return "impossible";
  }
  // Bare codex 0.147 survives a tool-router rejection and finishes its JSONL
  // turn. Older 0.145 sessions observed in #452 sometimes exited immediately
  // after the same router ERROR. That is an adapter-session fault, not evidence
  // about the task, so it spends the bounded machinery budget first.
  if (
    stage === "agent" &&
    effective &&
    effective.agent &&
    effective.agent.adapter === "codex" &&
    ((failure.node || {}).stderrExcerpt || "").includes("codex_core::tools::router")
  ) {
    return "machinery";
  }
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
  // the `steerable`/breach-tagging split below, which records it through the
  // existing diagnosis ledger already blocked at attempt 2.
  if (stage === "treeDelta") {
    return "breach";
  }
  // #424: the gate refusing to judge a pass is not the same event as the gate
  // catching a write, and must not be recorded under the other one's sentence.
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

function preparedSteering(task) {
  const comments = authorizedComments(task);
  const origin = args.steeringSource === undefined ? {} : {
    source: {
      kind: "local-jsonl",
      registrationId: args.steeringSource.registrationId,
      path: args.steeringSource.logPath,
      preparedCursor: args.steeringSource.preparedCursor,
      recheckedCursor: args.steeringSource.preparedCursor
    }
  };
  return {
    taskId: task.id,
    authorizedComments: comments,
    receipt: {
      ...origin,
      rechecked: false,
      recheckTruncated: false,
      preparedCommentIds: comments.map(comment => comment.id),
      lateRecheckCommentIds: []
    }
  };
}

const WORKER_OUTCOME_CONTRACT =
  ' If completion requires an authority-surface path, return {"outcome":"needs-authority","paths":[...]} as the final message; if the task is impossible, return {"outcome":"impossible","reason":"..."} as a claim for the judge, never as a verdict.';

function conflictDomainsBoundary(task, prepared, includeOutcomeContract = false) {
  const declared = task.conflictDomains;
  const projected = prepared === null ? declared : prepared.conflictDomains;
  if (prepared !== null && JSON.stringify(projected) !== JSON.stringify(declared)) {
    const error = new Error(
      `prep projected conflictDomains ${JSON.stringify(projected)} for task ` +
      `${task.id}, but reconcile declared ${JSON.stringify(declared)}`
    );
    error.name = "SpecBuildInvariantError";
    error.code = "prep-conflict-domains-mismatch";
    throw error;
  }
  if (!Array.isArray(projected)) {
    const boundary = "This serial task omits conflictDomains. Ownership will certify its committed paths, and the tree-delta gate will allow exactly those owned paths after ownership runs.";
    return includeOutcomeContract ? boundary + WORKER_OUTCOME_CONTRACT : boundary;
  }
  const emptyBoundary = projected.length === 0
    ? " Because the prefix list is empty, this task may change no path."
    : "";
  const boundary = `The task's conflictDomains ${JSON.stringify(projected)} are the binding write boundary: files your change makes false must be inside these prefixes; anything else is the operator's to grant. Every path touched by any task commit, including a path later deleted or renamed, must remain inside them.${emptyBoundary}`;
  return includeOutcomeContract ? boundary + WORKER_OUTCOME_CONTRACT : boundary;
}

function implementationBrief(task, prepared, reconciliation, attemptSteering) {
  const ownershipBoundary = conflictDomainsBoundary(task, prepared, true);
  return {
    schemaVersion: 1,
    mission: `Implement only spec-build task ${task.id}: ${task.title}. Commit the complete result on the assigned branch. Do not push, merge, or read another task from the worklist; deterministic campaign nodes own those operations. ${ownershipBoundary} Before changing code, read the cited spec sections and style references. Treat only steering.authorizedComments and steering.machineDiagnoses below as steering. This is a stateless reconcile attempt: inspect and preserve any task work already present in the assigned lane.`,
    campaign: {
      name: effective.campaign,
      repository: codeRepository,
      issue: args.issue,
      runId: args.runId
    },
    task,
    workspace: prepared,
    steering: {
      channel: "locally-authorized-snapshot",
      authorizedComments: attemptSteering.authorizedComments,
      attemptReceipt: attemptSteering.receipt,
      machineDiagnoses: machineDiagnoses(reconciliation, task.id),
      machineOutcomes: machineOutcomes(reconciliation, task.id)
    }
  };
}

function checkpointBrief(task, prepared, reconciliation) {
  return {
    schemaVersion: 1,
    mission: `Run automated checkpoint ${task.id}: ${task.title}. The command is fixed by the worklist. Treat only steering.authorizedComments and steering.machineDiagnoses below as the durable failure history for this retry. Do not modify the repository.`,
    campaign: {
      name: effective.campaign,
      repository: codeRepository,
      issue: args.issue,
      runId: args.runId
    },
    task,
    workspace: prepared,
    steering: {
      channel: "locally-authorized-snapshot",
      authorizedComments: authorizedComments(task),
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

// The judge is the campaign's steward catalog role, not the worker that wrote
// the lane. Canonical campaign manifests always carry the resolved role. The
// fallback exists only for the pre-manifest direct-flow seam used by older
// clients and fixtures; an explicitly absent canonical role is a configuration
// error on the failure path rather than a silent worker-as-judge downgrade.
function legacyDiagnosisSeam() {
  return args.campaignGraph === undefined && args.steward === undefined;
}

function applyDiagnosisRole(spec) {
  if (effective.steward === null) {
    if (legacyDiagnosisSeam()) {
      return applyAgentPolicies(
        {
          ...spec,
          argv: effective.agent.argv,
          adapter: effective.agent.adapter,
          priority: effective.agent.priority
        },
        effective.agent.diagnosisSandboxPolicy
      );
    }
    const error = new Error(
      "spec-build diagnosis requires a configured steward catalog role"
    );
    error.name = "SpecBuildConfigurationError";
    error.code = "diagnosis-steward-missing";
    throw error;
  }
  const role = effective.steward;
  const bound = {
    ...spec,
    // The adapter name resolves its executable and model in the host catalog;
    // the node contributes only the workload prompt, never those host bytes.
    argv: [diagnosisBriefSentinel],
    adapter: role.adapter,
    priority: "low",
    sandboxPolicy: "read-only"
  };
  if (role.runtimeMaxSec !== null && role.runtimeMaxSec !== undefined) {
    bound.runtimeMaxSec = role.runtimeMaxSec;
  }
  if (role.env !== undefined && Object.keys(role.env).length > 0) {
    bound.env = role.env;
  }
  return bound;
}

// Campaign-wide local records reach every task; a task-addressed record
// reaches only that stable task ID.
function authorizedComments(task) {
  const master = args.steering || [];
  if (args.taskSteering === undefined) {
    return master;
  }
  return master.concat(args.taskSteering[task.id] || []);
}

function reconciledProjection(reconciliation) {
  return {
    merged: reconciliation.merged,
    checkpoints: reconciliation.checkpoints,
    remaining: reconciliation.remaining,
    frontier: reconciliation.frontier.map(task => task.id),
    diagnoses: reconciliation.diagnoses,
    retries: reconciliation.retries,
    outcomes: reconciliation.outcomes,
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
    const identity = specBuildNodeIdentity(
      specBuildNodeRole.GATE,
      taskRef,
      key,
      key
    );
    return sh(gate.argv, {
      pools: ["campaign-control"],
      priority: "low",
      workspace,
      env: { CAMPAIGN_TASK_ID: task.id },
      runtimeMaxSec: gate.runtimeMaxSec,
      evidence: ["exit:0"],
      key: identity.key,
      label: identity.label,
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
  const taskRef = taskRefFor(task.id);
  const identity = specBuildNodeIdentity(
    specBuildNodeRole.GATE,
    taskRef,
    `preflight-gate-${gate.id}`,
    `preflight-gate-${gate.id}`
  );
  return sh(gate.preflightArgv, {
    pools: ["campaign-control"],
    priority: "low",
    workspace,
    env: { CAMPAIGN_TASK_ID: task.id },
    runtimeMaxSec: gate.runtimeMaxSec,
    evidence: ["exit:0"],
    key: identity.key,
    label: identity.label,
    settle: true,
    taskRef
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
  const taskRef = taskRefFor(task.id);
  const identity = specBuildNodeIdentity(
    specBuildNodeRole.GATE,
    taskRef,
    `preflight-witness-${gate.id}`,
    `preflight-witness-${gate.id}`
  );
  return sh(gate.argv, {
    pools: ["campaign-control"],
    priority: "low",
    workspace,
    env: { CAMPAIGN_TASK_ID: task.id },
    runtimeMaxSec: gate.runtimeMaxSec,
    key: identity.key,
    label: identity.label,
    settle: true,
    taskRef
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
      recovery: "start a fresh reconcile pass"
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
  const inputs = campaignInputs();
  if (seamSplit) {
    repositoryConfigFor(codeRepository);
    repositoryConfigFor(specRepository);
    repositoryConfigFor(issueRepository);
  }
  campaignTaskIdentity = args.campaignIdentity || inputs.campaign;
  effective = inputs;
  const configuredGateIds = [];
  for (const gate of effective.gates) {
    if (configuredGateIds.indexOf(gate.id) !== -1) {
      const error = new Error(`campaign gate id ${gate.id} is duplicated`);
      error.name = "SpecBuildConfigurationError";
      error.code = "duplicate-gate-id";
      throw error;
    }
    configuredGateIds.push(gate.id);
  }
  const captureRootSuffix = "/capture/archive";
  const tallyStateRoot = args.captureRoot.slice(0, -captureRootSuffix.length);
  attemptReceipts = {
    schemaVersion: 1,
    kind: "local-jsonl",
    path: `${tallyStateRoot}/campaigns/attempt-receipts/${effective.campaign}/attempt-receipts-v1.jsonl`
  };
  if (!effective.repositoryConfig) {
    const error = new Error(`campaign repository ${codeRepository} is not configured`);
    error.name = "SpecBuildConfigurationError";
    error.code = "repository-not-configured";
    throw error;
  }
  const sweepNode = await sweepCampaign(effective.repositoryConfig);
  const deferred = sweepDeferral(sweepNode);
  if (deferred !== null) {
    return deferred;
  }
  const reconcileBrief = withSeam({
    campaign: effective.campaign,
    campaignIdentity: campaignTaskIdentity,
    repository: codeRepository,
    repositoryConfig: effective.repositoryConfig,
    issue: args.issue,
    worklist: inputs.worklist,
    maxTasks: inputs.maxTasks,
    maxParallel: inputs.maxParallel,
    attemptReceipts
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
  // Accept an older driver/fixture projection that predates structured worker
  // outcomes. A real current driver always emits the field, while treating its
  // absence as the empty set preserves the legacy no-envelope path exactly.
  const reconciliation = {
    ...reconciliationNode.result,
    outcomes: reconciliationNode.result.outcomes || []
  };
  const repositoryConfig = effective.repositoryConfig;
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
      outcomes: [],
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
      outcomes: reconciliation.outcomes,
      deferrals: [],
      continuation: null,
      escalation
    };
  }
  // A marked integration commit is the durable proof that an earlier pass
  // cleared admission. Until that first merge exists, every fresh pass gates a
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
      campaignIdentity: campaignTaskIdentity,
      repository: codeRepository,
      repositoryConfig,
      issue: args.issue,
      runId: args.runId,
      workspaceRoot: args.workspaceRoot,
      task,
      // A lane forks from the local integration history the reconciler
      // witnessed; that branch advances without moving the shared remote base.
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
      const checkpointIdentity = specBuildNodeIdentity(
        specBuildNodeRole.GATE,
        taskRef,
        `checkpoint-${task.id}`,
        `checkpoint-${task.id}`
      );
      const checkpoint = await sh(task.argv, {
        pools: ["campaign-control"],
        priority: "low",
        workspace,
        env: { CAMPAIGN_TASK_ID: task.id },
        runtimeMaxSec: task.runtimeMaxSec,
        evidence: ["exit:0"],
        brief: taskBrief,
        key: checkpointIdentity.key,
        label: checkpointIdentity.label,
        settle: true,
        taskRef
      });
      const gateOutputs = [
        { phase: "checkpoint", gateId: task.id, kind: "checkpoint", node: checkpoint }
      ];
      // Record every terminal attempt, red or green. The command node's raw
      // files are private executor state; this deterministic node snapshots
      // their final 8 KiB into capture/archive before cleanup removes the lane.
      const execution = {
        taskUuid: checkpoint.taskUuid,
        verdict: checkpoint.verdict,
        exitCode: checkpoint.exitCode === undefined ? null : checkpoint.exitCode
      };
      const recorded = await driverNode(
        "checkpoint",
        withSeam({
          campaign: effective.campaign,
          campaignIdentity: campaignTaskIdentity,
          repository: codeRepository,
          repositoryConfig,
          issue: args.issue,
          task,
          source: reconciliation.source,
          baseRevision: reconciliation.baseRevision,
          workspace: prepared.result,
          captureRoot: args.captureRoot,
          execution
        }),
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
      const checkpointWithCapture = {
        ...checkpoint,
        capturePath: recorded.result.capturePath
      };
      if (checkpoint.verdict !== "pass") {
        return {
          task,
          prepared: prepared.result,
          failure: taskFailure(
            task,
            "checkpoint",
            checkpointWithCapture,
            taskBrief,
            [
              {
                phase: "checkpoint",
                gateId: task.id,
                kind: "checkpoint",
                node: checkpointWithCapture
              }
            ],
            prepared.result,
            prepared.result.baseRev
          )
        };
      }
      if (!recorded.result.passed || recorded.result.ref === null) {
        const error = new Error("a passing checkpoint produced no completion ref");
        error.name = "SpecBuildInvariantError";
        error.code = "checkpoint-completion-missing";
        throw error;
      }
      return { task, prepared: prepared.result, checkpoint: recorded.result };
    }

    let taskBrief = null;
    let assistedBy = null;
    let workerFindings = null;
    const prepSteering = preparedSteering(task);
    let attemptSteering = prepSteering;
    if (args.steeringSource !== undefined) {
      const recheckBrief = {
        campaign: effective.campaign,
        campaignIdentity: args.campaignIdentity,
        taskId: task.id,
        localActor: args.localActor,
        steeringSource: args.steeringSource,
        preparedComments: prepSteering.authorizedComments
      };
      // The append-only source is read once after lane preparation and
      // immediately before adapter admission. The prepared cursor plus this
      // fresh high-water read preserve the existing union/late-ID receipt;
      // both reads stay within the local append-only source.
      const recheckedSteering = await driverNode(
        "steeringRecheck",
        recheckBrief,
        `steering-recheck-${task.id}`,
        `steering-recheck-${task.id}`,
        steeringRecheckSchema,
        null,
        true,
        taskRef
      );
      if (!nodePassed(recheckedSteering)) {
        const failedTaskBrief = implementationBrief(
          task,
          prepared.result,
          reconciliation,
          prepSteering
        );
        return {
          task,
          prepared: prepared.result,
          failure: taskFailure(
            task,
            "steering:recheck",
            recheckedSteering,
            failedTaskBrief,
            [],
            prepared.result,
            prepared.result.baseRev
          )
        };
      }
      attemptSteering = recheckedSteering.result;
    }
    taskBrief = implementationBrief(
      task,
      prepared.result,
      reconciliation,
      attemptSteering
    );
    const agentIdentity = specBuildNodeIdentity(
      specBuildNodeRole.AGENT,
      taskRef,
      `agent-${task.id}`,
      `agent-${task.id}`
    );
    const agentSpec = applyAgentPolicies({
      argv: effective.agent.argv,
      adapter: effective.agent.adapter,
      pools: ["campaign-agent"],
      priority: effective.agent.priority,
      workspace,
      evidence: ["exit:0"],
      brief: taskBrief,
      key: agentIdentity.key,
      label: agentIdentity.label,
      taskRef
    });
    const agent = await job(agentSpec, { settle: true });
    if (agent.result !== undefined) {
      workerFindings = {
        taskUuid: agent.taskUuid,
        // The live client decodes JSON-looking final messages for structured
        // result consumers. Findings are a text channel, so retain that
        // value in its deterministic JSON spelling when decoding occurred.
        message:
          typeof agent.result === "string"
            ? agent.result
            : JSON.stringify(agent.result)
      };
    }
    const outcomeEnvelope = workerOutcomeEnvelope(agent.result);
    if (outcomeEnvelope !== null) {
      // A structured exit does not excuse a write outside the lane boundary.
      // The same detective gate used for a failing agent runs before the
      // receipt is trusted; a breach remains a breach, not a refusal.
      const declaresDomains = Array.isArray(task.conflictDomains);
      const outcomeDeltaBrief = {
        task,
        workspace: prepared.result,
        // A serial task without a declared boundary can still stop cleanly: an
        // empty owned-path fallback proves it made no change before refusing.
        ownershipRan: !declaresDomains
      };
      if (!declaresDomains) {
        outcomeDeltaBrief.ownedPaths = [];
      }
      const outcomeDelta = await driverNode(
        "treeDelta",
        outcomeDeltaBrief,
        `tree-delta-${task.id}`,
        `tree-delta-${task.id}`,
        treeDeltaSchema,
        workspace,
        true,
        taskRef
      );
      if (!nodePassed(outcomeDelta)) {
        return {
          task,
          prepared: prepared.result,
          failure: taskFailure(
            task,
            "treeDelta",
            outcomeDelta,
            taskBrief,
            [
              {
                phase: "treeDelta",
                gateId: "tree-delta",
                kind: "treeDelta",
                node: outcomeDelta
              }
            ],
            prepared.result,
            prepared.result.baseRev
          )
        };
      }
      const recordedOutcome = await driverNode(
        "outcome",
        {
          campaign: effective.campaign,
          issue: args.issue,
          task,
          taskUuid: agent.taskUuid,
          message: outcomeEnvelope,
          attemptReceipts
        },
        `outcome-${task.id}`,
        `outcome-${outcomeEnvelope.outcome}-${task.id}`,
        workerOutcomeRecordSchema,
        null,
        true,
        taskRef
      );
      if (!nodePassed(recordedOutcome)) {
        return {
          task,
          prepared: prepared.result,
          failure: taskFailure(
            task,
            "outcome:record",
            recordedOutcome,
            taskBrief,
            [],
            prepared.result,
            prepared.result.baseRev
          )
        };
      }
      const outcome = recordedOutcome.result;
      const failure = taskFailure(
        task,
        outcome.outcome,
        agent,
        taskBrief,
        [],
        prepared.result,
        prepared.result.baseRev
      );
      failure.outcome = outcome;
      failure.report = {
        taskId: task.id,
        stage: outcome.outcome,
        verdict: outcome.outcome === "impossible" ? "impossible-claim" : outcome.outcome,
        claim: outcome.outcome === "impossible",
        detail: outcome.outcome === "needs-authority"
          ? `requested paths: ${JSON.stringify(outcome.paths)}`
          : `worker impossibility claim: ${outcome.reason}`
      };
      return { task, prepared: prepared.result, failure };
    }
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
    assistedBy =
      agent.model === undefined || agent.model === null
        ? null
        : {
            adapter: effective.agent.adapter,
            model: agent.model,
            taskUuid: agent.taskUuid,
            witnessSeq: agent.witnessSeq
          };
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

    const publishBrief = withSeam({
      campaign: effective.campaign,
      campaignIdentity: campaignTaskIdentity,
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
      constraints: constraintResults,
      workerFindings
    });
    const publication = await driverNode(
      "publish",
      publishBrief,
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
      assistedBy,
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

  // Stable local snapshots are parallel; integration is deliberately ordered.
  // Before every merge the driver compares the tested base to the campaign's
  // integration branch. Only a moved branch causes a rebase and a second
  // witnessed gate pass.
  for (const lane of publications) {
    const task = lane.task;
    const taskRef = taskRefFor(task.id);
    const workspace = workspaceFor(lane.prepared);
    const integration = await driverNode(
      "rebase",
      {
        campaign: effective.campaign,
        campaignIdentity: campaignTaskIdentity,
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
      {
        campaign: effective.campaign,
        campaignIdentity: campaignTaskIdentity,
        repository: codeRepository,
        repositoryConfig,
        issue: args.issue,
        runId: args.runId,
        workspaceRoot: args.workspaceRoot,
        task,
        domainsRequired,
        mergeMethod: effective.mergeMethod,
        assistedBy: lane.assistedBy || null,
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
  const outcomes = [];
  for (const failure of failures) {
    const kind = failureClass(reconciliation, failure);
    if (kind === "needs-authority") {
      // The durable outcome receipt blocks this exact task revision. It is
      // neither a diagnosis nor a machinery retry, so it spends no attempt.
      outcomes.push(failure.outcome);
    } else if (kind === "impossible") {
      // The worker supplied evidence, not judgment. Preserve the claim and
      // send it through the adversarial diagnosis path that decides what the
      // next machine action may be.
      outcomes.push(failure.outcome);
      failure.impossibleClaim = true;
      steerable.push(failure);
    } else if (kind === "work") {
      steerable.push(failure);
    } else if (kind === "breach") {
      // #386: shares the diagnose-and-record pipeline below (the path list
      // still reaches the steward's diagnose slot) but never the retry
      // budget -- `steerBrief.breach` makes the driver record both the
      // attempt-1 and attempt-2 diagnosis receipts atomically, so the task
      // is permanently blocked as of this pass rather than steered once and
      // retried.
      failure.breach = true;
      steerable.push(failure);
    } else if (kind === "ungated") {
      // #424: the gate could not judge this pass at all. It takes the breach
      // routing -- both receipts recorded at once, lane aborted, no steering
      // attempt spent as if the agent were at fault -- but it is tagged
      // separately, because "wrote outside its authorized paths" is not what
      // happened and the recorded receipt must not say it did.
      failure.breach = true;
      failure.ungated = true;
      steerable.push(failure);
    } else if (kind === "machinery") {
      machineryFaults.push(failure);
    } else {
      deferrals.push(failure);
    }
  }

  // A machinery fault buys a retry only while the task's receipt-counted retry
  // budget lasts. Once it is spent the fault is steered like any other failure,
  // so a permanently broken lane still reaches escalation instead of looping.
  const retryOutcomes = await parallel(
    machineryFaults.map(failure => () => (async () => {
      const task = failure.task;
      const retryBrief = withSeam({
        campaign: effective.campaign,
        repository: codeRepository,
        repositoryConfig,
        issue: args.issue,
        taskId: task.id,
        stage: failure.stage,
        attemptReceipts,
        detail: bounded(
          failure.node && failure.node.error ? failure.node.error : failure.node,
          1500
        )
      });
      if (
        task.kind === "checkpoint" &&
        failure.node &&
        typeof failure.node.capturePath === "string"
      ) {
        retryBrief.checkpointCapture = {
          path: failure.node.capturePath,
          postFailureEvidence: effective.postFailureEvidence,
          postFailureStderr: effective.postFailureStderr
        };
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
      // The validator below requires literal gate evidence, so derive the
      // exact same evidence before the model runs and put its required strings
      // in the model's mission. A rule disclosed only to the validator turns
      // correct paraphrases into silent steering loss.
      const gateEvidence = gateEvidenceForFailure(failure);
      const literalSubstringRule = diagnosisLiteralSubstringRule(gateEvidence);
      const historyWalkRule = forbidPathsHistoryRule(gateEvidence);
      const ownershipBoundary = task.kind === "implementation"
        ? ` ${conflictDomainsBoundary(task, failure.prepared)}`
        : "";
      const verdictContract =
        " Return exactly one diagnosis result object. Set verdict to retry only for an " +
        "actionable fix wholly inside this implementation task's authorized paths; that " +
        "diagnosis becomes attempt 2 steering. Set verdict to blocked for an out-of-task " +
        "cause such as missing authority, a gate-contract fix, a dependency, or a source " +
        "fix elsewhere; blocked stops after this attempt and notifies the operator. Set " +
        "verdict to transient only for a machinery or session fault; transient consumes " +
        "the existing bounded machinery-retry budget. A needs-authority worker envelope " +
        "is always blocked before diagnosis. Checkpoint tasks never retry, and no verdict " +
        "can exceed the hard two-attempt or lifetime caps. The diagnosis field must begin " +
        "with one outcome-first sentence whose first word is a past-tense verb, end that " +
        "sentence with a period or colon before any list, contain no exclamation marks, " +
        "and stay under 12,000 characters. Include proposal only with a blocked verdict " +
        "when an actionable worklist or authority fix exists. A proposal kind is exactly " +
        "amendment-task or gate-set-fix; paths must be unique normalized repository-relative " +
        "paths with no empty, dot, or dot-dot component (at most 128 paths and 4,096 " +
        "characters each); goal states the desired result in at most 12,000 characters; " +
        "there are 1 to 16 acceptance criteria, each with a unique safe id of at most 80 " +
        "characters, a description of at most 4,000 characters, and 1 to 32 argv strings " +
        "of at most 4,096 characters each; and dependencies contains at most 128 unique " +
        "stable task IDs of at most 80 characters. Do not include credentials, tokens, " +
        "or other secret-looking values in either diagnosis or proposal.";
      const diagnosisBrief = {
        schemaVersion: 1,
        role: "diagnosis",
        // #386: a breach has no next attempt -- the lane is aborted, not
        // retried -- so the mission asks for a record of what happened
        // rather than steering for a redispatch that will never come. #424:
        // an unjudgeable pass is aborted for the same reason but is not the
        // same event, and asking a model to explain paths that were never
        // named would be asking it to invent them.
        mission: (
          failure.impossibleClaim
            ? `Worker task ${task.id} returned an impossibility claim. Assess that claim independently; the worker does not grade its own exit, so treat the claim as evidence rather than proof. Do not modify the repository.`
            : failure.ungated
            ? `Task ${task.id} could not be judged by the tree-delta permission gate and its lane is being aborted, not retried: its agent node failed, so the ownership node never ran and certified no paths, and the task declares no conflictDomains, leaving no allowlist to judge its worktree against. No out-of-allowlist change has been established. Return a concise blocked record for the operator. Do not modify the repository.`
            : failure.breach
            ? `Task ${task.id} wrote outside its authorized paths and its lane is being aborted, not retried. Return a concise blocked record of what the out-of-allowlist change(s) were and why they likely happened. Do not modify the repository.`
            : `Judge failed spec-build task ${task.id} independently from the worker that attempted it. Return a concise diagnosis and the typed verdict that deterministic machinery must execute. Do not modify the repository.`
        ) + verdictContract + ownershipBoundary + literalSubstringRule + historyWalkRule,
        campaign: {
          name: effective.campaign,
          repository: codeRepository,
          issue: args.issue,
          runId: args.runId
        },
        task,
        failure: {
          stage: failure.stage,
          verdict: failure.impossibleClaim
            ? "impossible-claim"
            : failure.node && failure.node.verdict
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
        workerOutcomeClaim: failure.impossibleClaim ? failure.outcome : null,
        previousWorkerOutcomes: machineOutcomes(reconciliation, task.id),
        gateOutputs: failure.gateOutputs,
        taskBrief: failure.taskBrief,
        diff,
        previousDiagnoses
      };
      // The diagnosis brief prohibits mutation, so the node is sandboxed to
      // match rather than inheriting the implementation node's writable policy.
      const diagnosisIdentity = specBuildNodeIdentity(
        specBuildNodeRole.DIAGNOSIS,
        taskRef,
        `diagnose-${task.id}`,
        `diagnose-${task.id}`
      );
      const diagnosisSpec = applyDiagnosisRole(
        {
          pools: ["campaign-agent"],
          evidence: ["exit:0"],
          brief: diagnosisBrief,
          key: diagnosisIdentity.key,
          label: diagnosisIdentity.label,
          taskRef,
          resultSchema: legacyDiagnosisSeam()
            ? legacyDiagnosisResultSchema
            : diagnosisResultSchema
        }
      );
      if (failure.prepared !== null && diff.available) {
        diagnosisSpec.workspace = workspaceFor(
          failure.prepared,
          failure.baseRev || failure.prepared.baseRev
        );
      }
      const diagnosed = await job(diagnosisSpec, { settle: false });
      const attempt = previousDiagnoses.length + 1;
      // Old scripted flow clients returned the pre-verdict string directly.
      // Production diagnosis nodes are schema-forced to the object above; the
      // compatibility arm keeps those clients useful without weakening the
      // model-facing result schema.
      const diagnosisResult = typeof diagnosed.result === "string"
        ? {
            verdict: failure.breach || attempt === 2
              ? "blocked"
              : "retry",
            diagnosis: diagnosed.result
          }
        : diagnosed.result;
      // #386: a breach carries its own deterministic evidence -- the paths
      // the tree-delta gate named in its own failure -- straight into the
      // durable receipt, so the offending paths are witnessed regardless of
      // what the steward's diagnosis says.
      const steerBrief = withSeam({
        campaign: effective.campaign,
        repository: codeRepository,
        repositoryConfig,
        issue: args.issue,
        taskId: task.id,
        attempt,
        diagnosis: diagnosisResult.diagnosis,
        ...(legacyDiagnosisSeam()
          ? {}
          : {
              taskKind: task.kind,
              stage: failure.stage,
              verdict: diagnosisResult.verdict,
              ...(diagnosisResult.proposal === undefined
                ? {}
                : { proposal: diagnosisResult.proposal })
            }),
        attemptReceipts,
        ...(gateEvidence ? { gateEvidence } : {}),
        ...(failure.breach
          ? {
              breach: true,
              breachDetail: bounded(
                failure.node && failure.node.error ? failure.node.error : failure.node,
                2000
              ),
              // #424: which abort this is. The driver composes a different
              // label sentence for each, because the local receipt must claim
              // exactly what happened --
              // a gate that could not judge is not a gate that caught a write.
              ...(failure.ungated ? { abortReason: "tree-delta-ungated" } : {})
            }
          : {})
      });
      if (
        task.kind === "checkpoint" &&
        failure.node &&
        typeof failure.node.capturePath === "string"
      ) {
        steerBrief.checkpointCapture = {
          path: failure.node.capturePath,
          postFailureEvidence: effective.postFailureEvidence,
          postFailureStderr: effective.postFailureStderr
        };
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
  const steeringResults = diagnosisOutcomes
    .filter(outcome => outcome.ok)
    .map(outcome => outcome.value);
  const diagnoses = steeringResults.filter(result => result.kind === "diagnosis");
  retries.push(...steeringResults.filter(result => result.kind === "retry"));
  retries.push(
    ...steeringResults
      .map(result => result.retry)
      .filter(result => result !== null && result !== undefined && result.posted)
  );
  let terminalError = diagnosisFailure ? diagnosisFailure.error : retryError;
  const advanced =
    merged.length > 0 ||
    checkpoints.length > 0 ||
    diagnoses.length > 0 ||
    retries.length > 0 ||
    outcomes.length > 0 ||
    deferrals.length > 0;
  if (terminalError === null && !advanced) {
    const error = new Error(
      "a non-quiescent campaign frontier produced no merge, checkpoint, worker outcome, retry, or machine steering"
    );
    error.name = "SpecBuildInvariantError";
    error.code = "frontier-without-outcome";
    terminalError = error;
  }

  // The continuation is written even when the steering lane threw. A transient
  // adapter fault must not leave the campaign stopped with no local successor.
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
        brief: args
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
        .concat(checkpoints, diagnoses, retries, outcomes)
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
    // one of these durable outcomes holds.
    state: merged.length > 0 || checkpoints.length > 0
      ? "advanced"
      : diagnoses.some(diagnosis => diagnosis.verdict === "retry")
        ? "steered"
        : retries.length > 0
          ? "retrying"
          : !legacyDiagnosisSeam() && diagnoses.some(
            diagnosis => diagnosis.verdict === "blocked" || diagnosis.blocked
          )
            ? "blocked"
        : outcomes.some(outcome => outcome.outcome === "needs-authority")
          ? "needs-authority"
          : "steered",
    reconciled: reconciledProjection(reconciliation),
    maintenance: sweepNode.result,
    checkpoints,
    merged,
    failures: failures.map(failure => failure.report || failure),
    diagnoses,
    retries,
    outcomes,
    deferrals: deferrals.map(failure => failure.task.id),
    continuation,
    escalation: null
  };
})();
