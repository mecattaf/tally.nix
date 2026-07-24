export const meta = {
  name: "fixture-banned-global",
  description: "must fail",
  pools: ["build"],
  argsSchema: { type: "object" }
};

Math.random();
