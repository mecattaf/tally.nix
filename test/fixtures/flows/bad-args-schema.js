export const meta = {
  name: "fixture-bad-args-schema",
  description: "must fail",
  pools: ["build"],
  argsSchema: {
    type: "definitely-not-a-json-schema-type"
  }
};
