export const meta = {
  name: "fixture-undeclared-pool",
  description: "must fail",
  pools: ["build"],
  argsSchema: { type: "object" }
};

job({ argv: ["true"], pools: ["worker-gpu"] });
