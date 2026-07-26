export const meta = {
  name: "fixture-multi-host",
  description: "SSH execution, replay attach, and Git- and Attic-backed cross-host artifact handoff",
  pools: ["coordinator-slot", "worker-slot"],
  argsSchema: {
    type: "object",
    required: ["workerProgram", "coordinatorProgram"],
    properties: {
      workerProgram: { type: "string", pattern: "^/nix/store/" },
      coordinatorProgram: { type: "string", pattern: "^/nix/store/" }
    },
    additionalProperties: false
  },
  maxNodes: 2,
  selectors: []
};

(async () => {
  const worker = await sh([args.workerProgram], {
    pools: ["worker-slot"],
    executor: "worker",
    key: "worker-artifact",
    runtimeMaxSec: 90,
    evidence: ["exit:0"],
    label: "worker-artifact"
  });
  const coordinator = await sh([args.coordinatorProgram], {
    pools: ["coordinator-slot"],
    key: "coordinator-consume",
    runtimeMaxSec: 30,
    evidence: ["exit:0"],
    label: "coordinator-consume"
  });
  return { worker, coordinator };
})();
