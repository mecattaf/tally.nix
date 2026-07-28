export const meta = {
  name: "fixture-sugar-pools",
  description: "must fail: claude() fixes its own pool",
  pools: ["claude-window"],
  argsSchema: { type: "object" }
};

claude("review the diff", { pools: ["claude-window"], key: "review" });
