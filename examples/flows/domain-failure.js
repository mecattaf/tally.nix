export const meta = {
  name: "domain-failure",
  description: "Express a domain outcome as a validated envelope instead of a thrown error",
  pools: ["worker-cpu"],
  argsSchema: {
    type: "object",
    required: ["invoice"],
    properties: {
      invoice: { type: "string", minLength: 1 }
    },
    additionalProperties: false
  },
  maxNodes: 3,
  selectors: []
};

// A custom `throw` in a flow script becomes FlowScriptError/script-evaluation and
// exits 10 — the same class as a typo or a null dereference. An operator reading
// exit 10 cannot tell "this invoice is unpayable" from "the script is broken".
//
// So a domain outcome is data: a discriminated envelope that resultSchema
// enforces. The node still passes, the run still succeeds, and the decision is
// in the witnessed result where a later node and an operator can both read it.
const decisionSchema = {
  type: "object",
  required: ["status"],
  oneOf: [
    {
      required: ["status", "amount"],
      properties: {
        status: { const: "payable" },
        amount: { type: "number", exclusiveMinimum: 0 }
      },
      additionalProperties: false
    },
    {
      required: ["status", "reason"],
      properties: {
        status: { const: "rejected" },
        reason: { type: "string", enum: ["duplicate", "unbalanced", "expired"] }
      },
      additionalProperties: false
    }
  ]
};

(async () => {
  const decision = await sh(["tally-invoice", "--check", args.invoice], {
    pools: ["worker-cpu"],
    key: "decide",
    label: "decide",
    evidence: ["exit:0"],
    resultSchema: decisionSchema
  });

  // Branching on the discriminant is ordinary control flow. Neither branch
  // throws, so neither is reported as a script defect.
  if (decision.result.status === "rejected") {
    await sh(["tally-invoice", "--file-rejection", args.invoice, decision.result.reason], {
      pools: ["worker-cpu"],
      key: "file-rejection",
      label: "file-rejection",
      evidence: ["exit:0"]
    });
    return { invoice: args.invoice, paid: false, reason: decision.result.reason };
  }

  const payment = await sh(
    ["tally-invoice", "--pay", args.invoice, String(decision.result.amount)],
    {
      pools: ["worker-cpu"],
      key: "pay",
      label: "pay",
      evidence: ["exit:0"]
    }
  );

  // A genuine infrastructure failure is still a failure: the node's own verdict
  // rejects the promise, and the run exits on the terminal error rather than
  // pretending the invoice was paid.
  return {
    invoice: args.invoice,
    paid: true,
    amount: decision.result.amount,
    witnessSeq: payment.witnessSeq
  };
})();
