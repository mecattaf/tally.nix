export const meta = {
  name: "fixture-args-from-brief",
  description: "large runner arguments remain outside argv",
  pools: [],
  argsSchema: {
    type: "object",
    required: ["marker", "body"],
    properties: {
      marker: { type: "string", minLength: 1 },
      body: { type: "string", minLength: 1 }
    },
    additionalProperties: false
  },
  maxNodes: 1
};

({ marker: args.marker, bodyBytes: args.body.length });
