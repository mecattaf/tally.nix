export const meta = {
  name: "fixture-nonliteral",
  description: "must fail",
  pools: ["build"],
  argsSchema: makeSchema()
};

function makeSchema() {
  return { type: "object" };
}
