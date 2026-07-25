export const meta = {
  name: "pooled-review",
  description: "Diverse local review with one repair attempt and dissent-preserving reduction",
  pools: ["worker-gpu"],
  argsSchema: {
    type: "object",
    required: ["subject", "minimumValid"],
    properties: {
      subject: { type: "string", minLength: 1 },
      minimumValid: { type: "integer", minimum: 1, maximum: 3 }
    },
    additionalProperties: false
  },
  maxNodes: 8,
  selectors: ["pooled-strongest"]
};

(async () => {
  const selected = members("pooled-strongest", {
    count: 3,
    diversity: "family"
  });
  const requiredMembers = selected.map(member => member.id);
  const candidateSchema = {
    type: "object",
    required: ["recommendation", "evidence"],
    properties: {
      recommendation: { type: "string", minLength: 1 },
      evidence: {
        type: "array",
        items: { type: "string", minLength: 1 }
      }
    },
    additionalProperties: false
  };
  const initial = await parallel(
    selected.map(member => () =>
      local(`Review this subject and cite concrete evidence: ${args.subject}`, {
        member,
        settle: true,
        resultSchema: candidateSchema,
        label: `review-${member.id}`
      })
    ),
    { settle: true }
  );
  const rows = initial.map((outcome, index) =>
    attributed(selected[index], outcome)
  );

  let assessed;
  try {
    assessed = quorum({
      results: rows,
      minimumValid: args.minimumValid,
      requiredMembers,
      allowPartial: true
    });
  } catch (error) {
    if (error.code !== "quorum-not-met") {
      throw error;
    }
    assessed = error.quorum;
  }
  const repairIds = assessed.invalid
    .concat(assessed.missing)
    .map(row => row.memberId);
  for (const memberId of repairIds) {
    const member = selected.find(candidate => candidate.id === memberId);
    const repaired = await local(
      `Return only the required review contract for: ${args.subject}`,
      {
        member,
        key: repairKey(member),
        settle: true,
        resultSchema: candidateSchema,
        label: `repair-${member.id}`
      }
    );
    const index = requiredMembers.indexOf(memberId);
    rows[index] = attributed(member, repaired);
  }

  const accepted = quorum({
    results: rows,
    minimumValid: args.minimumValid,
    requiredMembers,
    allowPartial: true
  });
  const reducerSchema = {
    type: "object",
    required: ["conclusions"],
    properties: {
      conclusions: {
        type: "array",
        minItems: 1,
        items: {
          type: "object",
          required: ["conclusion", "support", "conflict"],
          properties: {
            conclusion: { type: "string", minLength: 1 },
            support: {
              type: "array",
              minItems: 1,
              uniqueItems: true,
              items: { type: "string", minLength: 1 }
            },
            conflict: {
              type: "array",
              uniqueItems: true,
              items: { type: "string", minLength: 1 }
            }
          },
          additionalProperties: false
        }
      }
    },
    additionalProperties: false
  };
  const reducerBrief = `Reduce these attributed reviews without erasing dissent: ${JSON.stringify(
    accepted.valid
  )}`;
  let reducer = await local(
    reducerBrief,
    {
      member: selected[0],
      key: "dissent-reducer",
      settle: true,
      resultSchema: reducerSchema,
      label: "dissent-reducer"
    }
  );
  if (reducer.verdict !== "pass" || reducer.error) {
    reducer = await local(
      `Repair the reducer contract and preserve the same evidence: ${reducerBrief}`,
      {
        member: selected[0],
        key: "dissent-reducer@1",
        resultSchema: reducerSchema,
        label: "dissent-reducer-repair"
      }
    );
  }
  return {
    quorum: accepted,
    reduction: dissent({
      conclusions: reducer.result.conclusions,
      excluded: accepted.invalid
        .concat(accepted.missing)
        .map(row => ({ memberId: row.memberId, reason: row.status }))
    })
  };
})();
