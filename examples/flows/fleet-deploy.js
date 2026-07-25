export const meta = {
  name: "fleet-deploy",
  description: "Zero-LLM deployment with cross-host handoff through Git branches",
  pools: ["coordinator-build", "worker-deploy"],
  argsSchema: {
    type: "object",
    required: [
      "remote",
      "revision",
      "coordinatorCheckout",
      "workerCheckout"
    ],
    properties: {
      remote: { type: "string", minLength: 1 },
      revision: { type: "string", minLength: 1 },
      coordinatorCheckout: { type: "string", pattern: "^/" },
      workerCheckout: { type: "string", pattern: "^/" }
    },
    additionalProperties: false
  },
  maxNodes: 5,
  selectors: []
};

(async () => {
  await sh(
    [
      "git",
      "-C",
      args.coordinatorCheckout,
      "push",
      args.remote,
      `${args.revision}:refs/heads/tally-deploy`
    ],
    {
      pools: ["coordinator-build"],
      key: "publish-deployment",
      evidence: ["exit:0"],
      label: "publish-deployment"
    }
  );
  await sh(
    [
      "git",
      "-C",
      args.workerCheckout,
      "fetch",
      args.remote,
      "refs/heads/tally-deploy"
    ],
    {
      pools: ["worker-deploy"],
      executor: "worker",
      key: "worker-fetch",
      evidence: ["exit:0"],
      label: "worker-fetch"
    }
  );
  await sh(
    ["git", "-C", args.workerCheckout, "checkout", "--detach", "FETCH_HEAD"],
    {
      pools: ["worker-deploy"],
      executor: "worker",
      key: "worker-deploy",
      evidence: ["exit:0"],
      label: "worker-deploy"
    }
  );
  await sh(
    [
      "git",
      "-C",
      args.workerCheckout,
      "push",
      args.remote,
      "HEAD:refs/heads/tally-deployed"
    ],
    {
      pools: ["worker-deploy"],
      executor: "worker",
      key: "publish-deployment-receipt",
      evidence: ["exit:0"],
      label: "publish-deployment-receipt"
    }
  );
  return sh(
    [
      "git",
      "-C",
      args.coordinatorCheckout,
      "fetch",
      args.remote,
      "refs/heads/tally-deployed"
    ],
    {
      pools: ["coordinator-build"],
      key: "verify-deployment-receipt",
      evidence: ["exit:0"],
      label: "verify-deployment-receipt"
    }
  );
})();
