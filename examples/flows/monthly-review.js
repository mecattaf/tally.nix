export const meta = {
  name: "monthly-review",
  description: "Monthly local-model roster review with store-native stages and attributed quorum",
  pools: ["local-ai-review", "coordinator-gpu"],
  argsSchema: {
    type: "object",
    required: [
      "minimumValid",
      "publish",
      "dotfilesUrl",
      "baseBranch",
      "driver"
    ],
    properties: {
      minimumValid: { type: "integer", minimum: 1, maximum: 3 },
      publish: { type: "boolean" },
      dotfilesUrl: { type: "string", minLength: 1 },
      baseBranch: {
        type: "string",
        pattern: "^[A-Za-z0-9._/+-]+$",
        minLength: 1
      },
      period: {
        type: "string",
        pattern: "^[0-9]{4}-[0-9]{2}$"
      },
      driver: {
        type: "object",
        required: [
          "adapter",
          "program",
          "stateDir",
          "receiptPath",
          "runtimeMaxSec"
        ],
        properties: {
          adapter: { type: "string", minLength: 1 },
          program: { type: "string", pattern: "^/" },
          stateDir: { type: "string", pattern: "^/" },
          receiptPath: { type: "string", pattern: "^/" },
          runtimeMaxSec: { type: "integer", minimum: 1 }
        },
        additionalProperties: false
      }
    },
    additionalProperties: false
  },
  maxNodes: 16,
  selectors: ["pooled-strongest"]
};

const drvSchema = {
  type: "object",
  required: ["drvPath", "outputs"],
  properties: {
    drvPath: {
      type: "string",
      pattern: "^/nix/store/[0-9a-z]{32}-[^/]+[.]drv$"
    },
    outputs: {
      type: "array",
      minItems: 1,
      items: {
        type: "object",
        required: ["name", "path"],
        properties: {
          name: { type: "string", minLength: 1 },
          path: {
            type: "string",
            pattern: "^/nix/store/[0-9a-z]{32}-[^/]+$"
          }
        },
        additionalProperties: false
      }
    }
  },
  additionalProperties: false
};

const captureSchema = {
  type: "object",
  required: [
    "period",
    "changedCount",
    "dotfilesCommit",
    "runDir",
    "receiptPath",
    "commentaryPath",
    "preparation"
  ],
  properties: {
    period: { type: "string", pattern: "^[0-9]{4}-[0-9]{2}$" },
    changedCount: { type: "integer", minimum: 0 },
    dotfilesCommit: { type: "string", pattern: "^[0-9a-f]{40}$" },
    runDir: { type: "string", pattern: "^/" },
    receiptPath: { type: "string", pattern: "^/" },
    commentaryPath: { type: "string", pattern: "^/" },
    preparation: drvSchema
  },
  additionalProperties: false
};

const enrichSchema = {
  type: "object",
  required: [
    "evidenceDigest",
    "provider",
    "modelId",
    "endpoint",
    "modelTimeoutSec",
    "promptPath",
    "evidencePath",
    "contextPath",
    "hfMetadataPath",
    "enrichment"
  ],
  properties: {
    evidenceDigest: { type: "string", pattern: "^[0-9a-f]{64}$" },
    provider: { type: "string", minLength: 1 },
    modelId: { type: "string", minLength: 1 },
    endpoint: { type: "string", minLength: 1 },
    modelTimeoutSec: { type: "integer", minimum: 1 },
    promptPath: { type: "string", pattern: "^/" },
    evidencePath: { type: "string", pattern: "^/" },
    contextPath: { type: "string", pattern: "^/" },
    hfMetadataPath: { type: "string", pattern: "^/" },
    enrichment: drvSchema
  },
  additionalProperties: false
};

const commentarySchema = {
  type: "string",
  minLength: 40,
  maxLength: 50000
};

const reducerSchema = {
  type: "object",
  required: ["commentary", "conclusions"],
  properties: {
    commentary: { type: "string", minLength: 40, maxLength: 50000 },
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

const finalizeSchema = {
  type: "object",
  required: ["commentaryPath", "finalization"],
  properties: {
    commentaryPath: { type: "string", pattern: "^/" },
    finalization: drvSchema
  },
  additionalProperties: false
};

const publicationSchema = {
  type: "object",
  required: [
    "status",
    "period",
    "branch",
    "title",
    "changedFile",
    "prUrl"
  ],
  properties: {
    status: {
      enum: ["census", "preview", "published"]
    },
    period: { type: "string", pattern: "^[0-9]{4}-[0-9]{2}$" },
    branch: { type: "string", minLength: 1 },
    title: { type: "string", minLength: 1 },
    changedFile: {
      const: "pkgs/local-ai-monthly/sources.json"
    },
    prUrl: {
      type: ["string", "null"]
    }
  },
  additionalProperties: false
};

const failureSchema = {
  type: "object",
  required: ["status", "receiptPath"],
  properties: {
    status: { const: "failed" },
    receiptPath: { type: "string", pattern: "^/" }
  },
  additionalProperties: false
};

function driverNode(action, brief, options) {
  const nodeOptions = options || {};
  return job(
    {
      argv: [args.driver.program, action],
      adapter: args.driver.adapter,
      pools: ["local-ai-review"],
      priority: "low",
      runtimeMaxSec: args.driver.runtimeMaxSec,
      evidence: nodeOptions.evidence || ["exit:0"],
      brief,
      key: `driver-${action}`,
      label: `monthly-review-${action}`,
      resultSchema: nodeOptions.resultSchema
    },
    { settle: nodeOptions.settle === true }
  );
}

function memberPrompt(enriched, repair) {
  const instruction = repair
    ? "The previous response was invalid. Take the one allowed repair attempt and return only proposed pull-request commentary."
    : "Write only the proposed pull-request commentary now.";
  return [
    "Perform the monthly local-model roster review.",
    `Follow the review instructions in ${enriched.promptPath}.`,
    `Use the source evidence in ${enriched.evidencePath}.`,
    `Use the accepted-state context in ${enriched.contextPath}.`,
    `Use the bounded Hugging Face metadata in ${enriched.hfMetadataPath}.`,
    instruction
  ].join("\n");
}

function runMember(member, index, captured, enriched, repair) {
  const base = `local-ai-judge-${captured.period}-${enriched.evidenceDigest.slice(0, 20)}`;
  const suffix = repair
    ? `-${member.id}@1`
    : index === 0
      ? ""
      : `-${member.id}`;
  return local(memberPrompt(enriched, repair), {
    member,
    dedupKey: `${base}${suffix}`,
    priority: "low",
    runtimeMaxSec: enriched.modelTimeoutSec,
    evidence: ["exit:0"],
    env: {
      LLAMA_SWAP_URL: enriched.endpoint,
      PI_CODING_AGENT_DIR: `${captured.runDir}/pi-state/${member.id}`
    },
    settle: true,
    resultSchema: commentarySchema,
    label: repair ? `repair-${member.id}` : `review-${member.id}`
  });
}

function assess(rows, selected) {
  return quorum({
    results: rows,
    minimumValid: args.minimumValid,
    requiredMembers: selected.map(member => member.id),
    allowPartial: true
  });
}

function reducerValue(node, validMemberIds) {
  if (
    !node ||
    node.verdict !== "pass" ||
    node.error ||
    !node.result
  ) {
    return null;
  }
  for (const conclusion of node.result.conclusions) {
    const attributed = conclusion.support.concat(conclusion.conflict);
    if (attributed.some(memberId => !validMemberIds.includes(memberId))) {
      return null;
    }
  }
  return node.result;
}

function renderCommentary(accepted, reduction, reducerCommentary) {
  const sections = [reducerCommentary.trim(), "", "## Per-model reviews", ""];
  for (const row of accepted.valid) {
    sections.push(`### ${row.memberId}`, "", row.result.result.trim(), "");
  }
  sections.push("## Dissent ledger", "");
  for (const conclusion of reduction.conclusions) {
    sections.push(`- ${conclusion.conclusion}`);
    sections.push(`  - Support: ${conclusion.support.join(", ")}`);
    sections.push(
      `  - Conflict: ${
        conclusion.conflict.length === 0
          ? "none recorded"
          : conclusion.conflict.join(", ")
      }`
    );
  }
  if (reduction.excluded.length > 0) {
    sections.push("", "## Excluded members", "");
    for (const row of reduction.excluded) {
      sections.push(`- ${row.memberId}: ${row.reason}`);
    }
  }
  return `${sections.join("\n").trim()}\n`;
}

(async () => {
  let captured = null;
  try {
    captured = (
      await driverNode(
        "capture",
        {
          action: "capture",
          period: args.period || null,
          dotfilesUrl: args.dotfilesUrl,
          baseBranch: args.baseBranch,
          publish: args.publish,
          stateDir: args.driver.stateDir,
          receiptPath: args.driver.receiptPath
        },
        { resultSchema: captureSchema }
      )
    ).result;

    await drv(captured.preparation);

    const enriched = (
      await driverNode(
        "enrich",
        {
          action: "enrich",
          capture: captured,
          preparedPath: captured.preparation.outputs[0].path
        },
        { resultSchema: enrichSchema }
      )
    ).result;

    await drv(enriched.enrichment);

    const selected = members("pooled-strongest", {
      count: 3,
      diversity: "maker"
    });
    if (selected.length !== 3) {
      throw new Error(
        `pooled-strongest must resolve exactly three members, got ${selected.length}`
      );
    }
    const initial = await parallel(
      selected.map((member, index) => () =>
        runMember(member, index, captured, enriched, false)
      ),
      { settle: true }
    );
    const rows = initial.map((outcome, index) =>
      attributed(selected[index], outcome)
    );

    let firstAssessment;
    try {
      firstAssessment = assess(rows, selected);
    } catch (error) {
      if (error.code !== "quorum-not-met") {
        throw error;
      }
      firstAssessment = error.quorum;
    }
    const repairIds = firstAssessment.invalid
      .concat(firstAssessment.missing)
      .map(row => row.memberId);
    for (const memberId of repairIds) {
      const index = selected.findIndex(member => member.id === memberId);
      const repaired = await runMember(
        selected[index],
        index,
        captured,
        enriched,
        true
      );
      rows[index] = attributed(selected[index], repaired);
    }

    const accepted = assess(rows, selected);
    const validMemberIds = accepted.valid.map(row => row.memberId);
    const reducerBrief = [
      "Reduce these attributed monthly reviews into proposed pull-request commentary.",
      "Preserve disagreements. Every conclusion must name supporting and conflicting member IDs.",
      JSON.stringify(
        accepted.valid.map(row => ({
          memberId: row.memberId,
          commentary: row.result.result
        }))
      )
    ].join("\n");
    const reducerBase = `local-ai-reduce-${captured.period}-${enriched.evidenceDigest.slice(0, 20)}`;
    let reducer = await local(reducerBrief, {
      member: selected[0],
      dedupKey: reducerBase,
      priority: "low",
      runtimeMaxSec: enriched.modelTimeoutSec,
      evidence: ["exit:0"],
      env: {
        LLAMA_SWAP_URL: enriched.endpoint,
        PI_CODING_AGENT_DIR: `${captured.runDir}/pi-state/reducer`
      },
      settle: true,
      resultSchema: reducerSchema,
      label: "dissent-reducer"
    });
    let reduced = reducerValue(reducer, validMemberIds);
    if (reduced === null) {
      reducer = await local(
        `Repair the reducer contract once without changing its evidence:\n${reducerBrief}`,
        {
          member: selected[0],
          dedupKey: `${reducerBase}@1`,
          priority: "low",
          runtimeMaxSec: enriched.modelTimeoutSec,
          evidence: ["exit:0"],
          env: {
            LLAMA_SWAP_URL: enriched.endpoint,
            PI_CODING_AGENT_DIR: `${captured.runDir}/pi-state/reducer-repair`
          },
          settle: true,
          resultSchema: reducerSchema,
          label: "dissent-reducer-repair"
        }
      );
      reduced = reducerValue(reducer, validMemberIds);
    }
    if (reduced === null) {
      throw new Error("dissent reducer did not produce an attributed result after one repair");
    }

    const reduction = dissent({
      conclusions: reduced.conclusions,
      excluded: accepted.invalid
        .concat(accepted.missing)
        .map(row => ({ memberId: row.memberId, reason: row.status }))
    });
    const commentary = renderCommentary(
      accepted,
      reduction,
      reduced.commentary
    );

    const finalized = (
      await driverNode(
        "finalize",
        {
          action: "finalize",
          capture: captured,
          enriched,
          commentary,
          attribution: {
            selected: selected.map(member => member.id),
            valid: validMemberIds,
            dissent: reduction
          }
        },
        {
          evidence: [
            "exit:0",
            `artifact:${captured.commentaryPath}`,
            "hash:sha256"
          ],
          resultSchema: finalizeSchema
        }
      )
    ).result;

    await drv(finalized.finalization);

    const publication = (
      await driverNode(
        "publish",
        {
          action: "publish",
          capture: captured,
          enriched,
          finalizedPath: finalized.finalization.outputs[0].path,
          commentaryPath: finalized.commentaryPath,
          publish: args.publish,
          dotfilesUrl: args.dotfilesUrl,
          baseBranch: args.baseBranch
        },
        {
          evidence: [
            "exit:0",
            `artifact:${args.driver.receiptPath}`,
            "hash:sha256"
          ],
          resultSchema: publicationSchema
        }
      )
    ).result;

    return {
      publication,
      selected: selected.map(member => member.id),
      quorum: {
        minimumValid: accepted.minimumValid,
        valid: validMemberIds,
        excluded: reduction.excluded
      },
      dissent: reduction
    };
  } catch (error) {
    try {
      const failure = await driverNode(
        "failure",
        {
          action: "failure",
          capture: captured,
          stateDir: args.driver.stateDir,
          receiptPath: args.driver.receiptPath,
          error: {
            name: String((error && error.name) || "Error"),
            code: String((error && error.code) || "flow-failed"),
            message: String((error && error.message) || error)
          }
        },
        {
          evidence: [`artifact:${args.driver.receiptPath}`, "hash:sha256"],
          resultSchema: failureSchema,
          settle: true
        }
      );
      log({ failureReceiptVerdict: failure.verdict });
    } catch (receiptError) {
      log({
        failureReceiptError: String(
          (receiptError && receiptError.message) || receiptError
        )
      });
    }
    throw error;
  }
})();
