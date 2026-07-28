export const meta = {
  name: "worklist-fanout",
  description: "Bounded fan-out over a witnessed worklist with a settled culmination",
  pools: ["worker-cpu", "claude-window"],
  argsSchema: {
    type: "object",
    required: ["repository", "label", "waveSize"],
    properties: {
      repository: { type: "string", minLength: 1 },
      label: { type: "string", minLength: 1 },
      waveSize: { type: "integer", minimum: 1, maximum: 8 }
    },
    additionalProperties: false
  },
  // 1 worklist + 8 tasks + 8 repairs + 1 culmination.
  maxNodes: 18,
  iterationCap: 8,
  selectors: []
};

// The worklist node's brief and resultSchema together are the contract: nothing
// downstream reads free text, and a worker that answers off-contract fails here
// rather than corrupting every key derived from its answer.
const worklistSchema = {
  type: "object",
  required: ["items"],
  properties: {
    items: {
      type: "array",
      minItems: 1,
      maxItems: 8,
      items: {
        type: "object",
        required: ["id", "title"],
        properties: {
          id: { type: "string", pattern: "^[a-z0-9-]+$" },
          title: { type: "string", minLength: 1 }
        },
        additionalProperties: false
      }
    }
  },
  additionalProperties: false
};

const taskSchema = {
  type: "object",
  required: ["outcome"],
  properties: {
    outcome: { type: "string", enum: ["done", "blocked"] },
    note: { type: "string" }
  },
  additionalProperties: false
};

(async () => {
  const worklist = await sh(
    ["tally-worklist", "--repository", args.repository, "--label", args.label],
    {
      pools: ["worker-cpu"],
      key: "worklist",
      label: "worklist",
      evidence: ["exit:0"],
      resultSchema: worklistSchema
    }
  );

  // Every key below is derived from the witnessed worklist, so the second run of
  // this flow re-derives exactly the same keys and replays the admitted prefix.
  const items = worklist.result.items.slice(0, args.waveSize);

  // Combinator settle mode: one failed task must not suppress the culmination.
  // Node settle mode is separate — it turns a failed terminal into a resolved
  // value; this turns a rejecting branch into { ok: false, error }.
  const wave = await parallel(
    items.map(item => () =>
      sh(["tally-task", "--id", item.id], {
        pools: ["worker-cpu"],
        key: `task-${item.id}`,
        label: `task-${item.id}`,
        evidence: ["exit:0"],
        resultSchema: taskSchema
      })
    ),
    { settle: true }
  );

  // Repair headroom is budgeted in maxNodes, not discovered at run time. One
  // repair per failed item, and no repair of a repair.
  const repaired = await parallel(
    wave.map((outcome, index) => () => {
      if (outcome.ok) {
        return Promise.resolve(outcome);
      }
      return sh(["tally-task", "--id", items[index].id, "--repair"], {
        pools: ["worker-cpu"],
        key: `repair-${items[index].id}`,
        label: `repair-${items[index].id}`,
        evidence: ["exit:0"],
        resultSchema: taskSchema,
        settle: true
      }).then(result => ({ ok: true, value: result }));
    }),
    { settle: true }
  );

  const surviving = repaired
    .map((outcome, index) => ({
      id: items[index].id,
      title: items[index].title,
      value: outcome.ok && outcome.value.ok ? outcome.value.value : null
    }))
    .filter(row => row.value !== null && row.value.verdict === "pass");

  // The culmination runs on whatever survived, and names what did not.
  const missing = items
    .filter(item => !surviving.some(row => row.id === item.id))
    .map(item => item.id);

  const culmination = await claude(
    `Summarize this wave over ${args.repository}. Completed: ${JSON.stringify(
      surviving.map(row => ({ id: row.id, title: row.title }))
    )}. Unfinished: ${JSON.stringify(missing)}.`,
    {
      key: "culmination",
      label: "culmination",
      resultSchema: {
        type: "object",
        required: ["summary"],
        properties: { summary: { type: "string", minLength: 1 } },
        additionalProperties: false
      }
    }
  );

  return {
    completed: surviving.map(row => row.id),
    unfinished: missing,
    summary: culmination.result.summary
  };
})();
