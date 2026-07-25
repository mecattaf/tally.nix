export const meta = {
  name: "agency-nightly",
  description: "Plan, review, and verify one bounded overnight agency increment",
  pools: ["codex-window", "claude-window", "worker-build"],
  argsSchema: {
    type: "object",
    required: ["mission", "repository", "baseRev", "branch", "worktree"],
    properties: {
      mission: { type: "string", minLength: 1 },
      repository: { type: "string", minLength: 1 },
      baseRev: { type: "string", minLength: 1 },
      branch: { type: "string", minLength: 1 },
      worktree: { type: "string", pattern: "^/" }
    },
    additionalProperties: false
  },
  maxNodes: 3,
  selectors: []
};

(async () => {
  const workspace = {
    repo: args.repository,
    baseRev: args.baseRev,
    branch: args.branch,
    worktreePath: args.worktree
  };
  const plan = await codex(`Implement this bounded mission: ${args.mission}`, {
    key: "implementation",
    workspace,
    label: "implementation"
  });
  const review = await claude(
    `Review the implementation result and identify any blocking defect: ${JSON.stringify(
      plan.result
    )}`,
    {
      key: "review",
      workspace,
      label: "review"
    }
  );
  return sh(["git", "-C", args.worktree, "status", "--short"], {
    pools: ["worker-build"],
    key: "workspace-verification",
    workspace,
    brief: {
      mission: args.mission,
      implementation: plan.result,
      review: review.result
    },
    evidence: ["exit:0"],
    label: "workspace-verification"
  });
})();
