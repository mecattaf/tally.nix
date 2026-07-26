export const meta = {
  name: "fixture-valid",
  description: "valid flow-check fixture",
  pools: ["worker-gpu"],
  argsSchema: {
    type: "object",
    required: ["task"],
    properties: {
      task: { type: "string", minLength: 1 }
    },
    additionalProperties: false
  },
  maxNodes: 5,
  selectors: ["pooled-fast"]
};

const task = args.task;
task;
