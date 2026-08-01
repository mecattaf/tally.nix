export const meta = {
  name: "fixture-args-from-brief",
  description: "large runner arguments remain outside argv",
  pools: [],
  argsSchema: {
    type: "object",
    required: ["marker", "configBlob"],
    properties: {
      marker: { type: "string", minLength: 1 },
      configBlob: { type: "string", minLength: 1 }
    },
    additionalProperties: false
  },
  maxNodes: 1
};

({ marker: args.marker, configBytes: args.configBlob.length });
