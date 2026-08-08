export const meta = {
  name: "final-bar-reader-state",
  description: "one reusable node for archived-view aggregation",
  pools: ["stock"],
  argsSchema: { type: "object", additionalProperties: false },
  maxNodes: 1,
  selectors: []
};

(async () => sh(["/bin/sh", "-c", "true"], {
  pools: ["stock"],
  key: "final-bar-reader-state-node",
  evidence: ["exit:0"],
  label: "archive-node",
  taskRef: "final-bar/archive"
}))();
