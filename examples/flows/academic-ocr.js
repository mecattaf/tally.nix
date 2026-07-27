export const meta = {
  name: "academic-ocr",
  description: "Tiered academic OCR swarm with bounded input mutation and arbitration",
  pools: ["ocr-gpu"],
  argsSchema: {
    type: "object",
    required: [
      "pages",
      "protocols",
      "driver",
      "outputDir",
      "rasterDpi",
      "maxMutationIterations",
      "maxDisagreementPermille"
    ],
    properties: {
      pages: {
        type: "array",
        minItems: 1,
        maxItems: 100,
        items: {
          type: "object",
          required: ["paperId", "pageNumber", "sourcePath"],
          properties: {
            paperId: {
              type: "string",
              pattern: "^[A-Za-z0-9._-]+$",
              minLength: 1,
              maxLength: 80
            },
            pageNumber: { type: "integer", minimum: 1 },
            sourcePath: { type: "string", pattern: "^/" }
          },
          additionalProperties: false
        }
      },
      protocols: {
        type: "array",
        minItems: 2,
        maxItems: 4,
        items: {
          type: "object",
          required: ["id", "tier"],
          properties: {
            id: {
              type: "string",
              pattern: "^[A-Za-z0-9._-]+$",
              minLength: 1,
              maxLength: 80
            },
            tier: { enum: ["cheap", "standard", "specialist"] }
          },
          additionalProperties: false
        }
      },
      driver: {
        type: "object",
        required: ["adapter", "program", "runtimeMaxSec"],
        properties: {
          adapter: { type: "string", minLength: 1 },
          program: { type: "string", pattern: "^/" },
          runtimeMaxSec: { type: "integer", minimum: 1 }
        },
        additionalProperties: false
      },
      outputDir: { type: "string", pattern: "^/" },
      rasterDpi: { type: "integer", minimum: 200, maximum: 1200 },
      maxMutationIterations: { type: "integer", minimum: 1, maximum: 3 },
      maxDisagreementPermille: {
        type: "integer",
        minimum: 0,
        maximum: 1000
      }
    },
    additionalProperties: false
  },
  maxNodes: 1700,
  iterationCap: 1600,
  selectors: []
};

const tierOrder = ["cheap", "standard", "specialist"];

const hotZoneSchema = {
  type: "object",
  required: ["x", "y", "width", "height"],
  properties: {
    x: { type: "integer", minimum: 0, maximum: 10000 },
    y: { type: "integer", minimum: 0, maximum: 10000 },
    width: { type: "integer", minimum: 1, maximum: 10000 },
    height: { type: "integer", minimum: 1, maximum: 10000 }
  },
  additionalProperties: false
};

const recognitionSchema = {
  type: "object",
  required: [
    "paperId",
    "pageNumber",
    "protocolId",
    "inputVariant",
    "artifactPath",
    "textDigest",
    "signature",
    "confidencePermille",
    "hotZones",
    "skewMilliDegrees"
  ],
  properties: {
    paperId: { type: "string", minLength: 1 },
    pageNumber: { type: "integer", minimum: 1 },
    protocolId: { type: "string", minLength: 1 },
    inputVariant: { type: "string", minLength: 1 },
    artifactPath: { type: "string", pattern: "^/" },
    textDigest: { type: "string", pattern: "^sha256:[0-9a-f]{64}$" },
    signature: {
      type: "array",
      minItems: 1,
      maxItems: 32,
      items: { type: "integer", minimum: 0, maximum: 65535 }
    },
    confidencePermille: {
      type: "integer",
      minimum: 0,
      maximum: 1000
    },
    hotZones: {
      type: "array",
      maxItems: 8,
      items: hotZoneSchema
    },
    skewMilliDegrees: {
      type: "integer",
      minimum: -45000,
      maximum: 45000
    }
  },
  additionalProperties: false
};

const arbiterSchema = {
  type: "object",
  required: [
    "paperId",
    "pageNumber",
    "artifactPath",
    "textDigest",
    "basis"
  ],
  properties: {
    paperId: { type: "string", minLength: 1 },
    pageNumber: { type: "integer", minimum: 1 },
    artifactPath: { type: "string", pattern: "^/" },
    textDigest: { type: "string", pattern: "^sha256:[0-9a-f]{64}$" },
    basis: {
      type: "array",
      minItems: 1,
      uniqueItems: true,
      items: { type: "string", minLength: 1 }
    }
  },
  additionalProperties: false
};

function outputRoot() {
  if (args.outputDir === "/") {
    return "";
  }
  return args.outputDir.endsWith("/")
    ? args.outputDir.slice(0, args.outputDir.length - 1)
    : args.outputDir;
}

function pageStem(page) {
  return `${outputRoot()}/${page.paperId}/page-${page.pageNumber}`;
}

function recognitionArtifactPath(page, protocol, input) {
  return `${pageStem(page)}/${protocol.id}/${input.id}.json`;
}

function comparePages(left, right) {
  if (left.paperId !== right.paperId) {
    return left.paperId < right.paperId ? -1 : 1;
  }
  return left.pageNumber - right.pageNumber;
}

function compareProtocols(left, right) {
  const tierDifference =
    tierOrder.indexOf(left.tier) - tierOrder.indexOf(right.tier);
  if (tierDifference !== 0) {
    return tierDifference;
  }
  if (left.id === right.id) {
    return 0;
  }
  return left.id < right.id ? -1 : 1;
}

function validatedInputs() {
  const pages = args.pages.slice().sort(comparePages);
  for (let index = 1; index < pages.length; index += 1) {
    const previous = pages[index - 1];
    const current = pages[index];
    if (
      previous.paperId === current.paperId &&
      previous.pageNumber === current.pageNumber
    ) {
      throw new Error(
        `duplicate page ${current.paperId}/${current.pageNumber}`
      );
    }
  }

  const protocols = args.protocols.slice().sort(compareProtocols);
  for (let index = 1; index < protocols.length; index += 1) {
    if (protocols[index - 1].id === protocols[index].id) {
      throw new Error(`duplicate protocol ${protocols[index].id}`);
    }
  }

  const upperBound =
    pages.length *
      protocols.length *
      (1 + args.maxMutationIterations) +
    pages.length;
  if (upperBound > flowMeta.maxNodes) {
    throw new Error(
      `configured OCR swarm can materialize ${upperBound} nodes, above maxNodes ${flowMeta.maxNodes}`
    );
  }
  return { pages, protocols, upperBound };
}

function recognitionKey(page, protocol, input) {
  return [
    "ocr",
    page.paperId,
    page.pageNumber,
    protocol.id,
    input.id
  ].join("-");
}

async function recognize(page, protocol, input) {
  const artifactPath = recognitionArtifactPath(page, protocol, input);
  const result = await job(
    {
      argv: [args.driver.program, "recognize"],
      adapter: args.driver.adapter,
      pools: ["ocr-gpu"],
      priority: "low",
      runtimeMaxSec: args.driver.runtimeMaxSec,
      evidence: ["exit:0", `artifact:${artifactPath}`, "hash:sha256"],
      brief: {
        action: "recognize",
        page,
        protocol,
        input,
        artifactPath
      },
      key: recognitionKey(page, protocol, input),
      label: recognitionKey(page, protocol, input),
      resultSchema: recognitionSchema
    },
    { settle: true }
  );

  if (
    (result.verdict === "pass" || result.verdict === "substituted") &&
    !result.error
  ) {
    const summary = result.result;
    if (
      summary.paperId !== page.paperId ||
      summary.pageNumber !== page.pageNumber ||
      summary.protocolId !== protocol.id ||
      summary.inputVariant !== input.id ||
      summary.artifactPath !== artifactPath
    ) {
      throw new Error(
        `protocol ${protocol.id} returned a summary for the wrong page or input`
      );
    }
  }
  return result;
}

function successfulSummaries(results) {
  return results
    .filter(
      result =>
        (result.verdict === "pass" || result.verdict === "substituted") &&
        !result.error &&
        result.result
    )
    .map(result => ({ node: result, summary: result.result }));
}

function signatureDisagreement(left, right) {
  const length = Math.max(left.length, right.length);
  let differences = Math.abs(left.length - right.length);
  const shared = Math.min(left.length, right.length);
  for (let index = 0; index < shared; index += 1) {
    if (left[index] !== right[index]) {
      differences += 1;
    }
  }
  return Math.floor((differences * 1000) / length);
}

function pairKey(left, right) {
  return [left.summary.protocolId, right.summary.protocolId]
    .slice()
    .sort()
    .join("+");
}

function assess(results) {
  const candidates = successfulSummaries(results);
  let best = null;
  for (let left = 0; left < candidates.length; left += 1) {
    for (let right = left + 1; right < candidates.length; right += 1) {
      const disagreementPermille = signatureDisagreement(
        candidates[left].summary.signature,
        candidates[right].summary.signature
      );
      const key = pairKey(candidates[left], candidates[right]);
      if (
        best === null ||
        disagreementPermille < best.disagreementPermille ||
        (disagreementPermille === best.disagreementPermille && key < best.key)
      ) {
        best = {
          key,
          disagreementPermille,
          candidates: [candidates[left], candidates[right]]
        };
      }
    }
  }

  if (
    best === null ||
    best.disagreementPermille > args.maxDisagreementPermille
  ) {
    return {
      converged: false,
      disagreementPermille:
        best === null ? null : best.disagreementPermille,
      chosen: null,
      agreementProtocols: []
    };
  }

  const chosen = best.candidates
    .slice()
    .sort((left, right) => {
      const confidence =
        right.summary.confidencePermille - left.summary.confidencePermille;
      if (confidence !== 0) {
        return confidence;
      }
      if (left.summary.protocolId === right.summary.protocolId) {
        return 0;
      }
      return left.summary.protocolId < right.summary.protocolId ? -1 : 1;
    })[0];
  return {
    converged: true,
    disagreementPermille: best.disagreementPermille,
    chosen,
    agreementProtocols: best.candidates
      .map(candidate => candidate.summary.protocolId)
      .sort()
  };
}

async function runTiered(page, input, protocols) {
  let results = [];
  const tiersTried = [];
  for (const tier of tierOrder) {
    const members = protocols.filter(protocol => protocol.tier === tier);
    if (members.length === 0) {
      continue;
    }
    const tierResults = await parallel(
      members.map(protocol => () => recognize(page, protocol, input))
    );
    results = results.concat(tierResults);
    tiersTried.push(tier);
    const assessment = assess(results);
    if (assessment.converged) {
      return { input, results, tiersTried, assessment };
    }
  }
  return { input, results, tiersTried, assessment: assess(results) };
}

function uniqueHotZones(results) {
  const zones = [];
  for (const candidate of successfulSummaries(results)) {
    for (const zone of candidate.summary.hotZones) {
      zones.push(zone);
    }
  }
  zones.sort((left, right) => {
    for (const field of ["x", "y", "width", "height"]) {
      if (left[field] !== right[field]) {
        return left[field] - right[field];
      }
    }
    return 0;
  });
  return zones.filter((zone, index) => {
    if (index === 0) {
      return true;
    }
    const previous = zones[index - 1];
    return ["x", "y", "width", "height"].some(
      field => previous[field] !== zone[field]
    );
  }).slice(0, 8);
}

function medianSkew(results) {
  const skews = successfulSummaries(results)
    .map(candidate => candidate.summary.skewMilliDegrees)
    .sort((left, right) => left - right);
  if (skews.length === 0) {
    return 0;
  }
  return skews[Math.floor((skews.length - 1) / 2)];
}

function mutationAt(index, previousResults) {
  if (index === 0) {
    return {
      id: `rerasterize-${args.rasterDpi}-dpi`,
      mutation: { kind: "rerasterize", dpi: args.rasterDpi }
    };
  }
  if (index === 1) {
    return {
      id: "crop-hot-zones",
      mutation: {
        kind: "crop-hot-zones",
        zones: uniqueHotZones(previousResults)
      }
    };
  }
  return {
    id: "deskew",
    mutation: {
      kind: "deskew",
      correctionMilliDegrees: -medianSkew(previousResults)
    }
  };
}

function compactAttempt(result) {
  const summary = result.result;
  return {
    taskUuid: result.taskUuid,
    witnessSeq: result.witnessSeq,
    verdict: result.verdict,
    protocolId: summary ? summary.protocolId : null,
    inputVariant: summary ? summary.inputVariant : null,
    artifactPath: summary ? summary.artifactPath : null,
    textDigest: summary ? summary.textDigest : null
  };
}

function convergedPage(page, stage, attempts, resolution) {
  const chosen = stage.assessment.chosen;
  return {
    paperId: page.paperId,
    pageNumber: page.pageNumber,
    status: "converged",
    resolution,
    inputVariant: stage.input.id,
    chosenArtifactPath: chosen.summary.artifactPath,
    textDigest: chosen.summary.textDigest,
    disagreementPermille: stage.assessment.disagreementPermille,
    agreementProtocols: stage.assessment.agreementProtocols,
    attemptCount: attempts.length,
    proof: {
      taskUuid: chosen.node.taskUuid,
      witnessSeq: chosen.node.witnessSeq
    }
  };
}

async function arbitrate(page, attempts) {
  const artifactPath = `${pageStem(page)}/arbiter/final.json`;
  const result = await job({
    argv: [args.driver.program, "arbitrate"],
    adapter: args.driver.adapter,
    pools: ["ocr-gpu"],
    priority: "low",
    runtimeMaxSec: args.driver.runtimeMaxSec,
    evidence: ["exit:0", `artifact:${artifactPath}`, "hash:sha256"],
    brief: {
      action: "arbitrate",
      page,
      attempts: attempts.map(compactAttempt),
      artifactPath
    },
    key: `ocr-${page.paperId}-${page.pageNumber}-arbiter`,
    label: `ocr-${page.paperId}-${page.pageNumber}-arbiter`,
    resultSchema: arbiterSchema
  });
  if (
    result.result.paperId !== page.paperId ||
    result.result.pageNumber !== page.pageNumber ||
    result.result.artifactPath !== artifactPath
  ) {
    throw new Error("arbiter returned a summary for the wrong page");
  }
  return {
    paperId: page.paperId,
    pageNumber: page.pageNumber,
    status: "arbitrated",
    resolution: "arbiter",
    inputVariant: "arbiter",
    chosenArtifactPath: result.result.artifactPath,
    textDigest: result.result.textDigest,
    disagreementPermille: null,
    agreementProtocols: [],
    attemptCount: attempts.length,
    proof: {
      taskUuid: result.taskUuid,
      witnessSeq: result.witnessSeq
    }
  };
}

async function resolvePage(page, protocols) {
  const attempts = [];
  let stage = await runTiered(
    page,
    { id: "original", mutation: { kind: "none" } },
    protocols
  );
  attempts.push(...stage.results);
  if (stage.assessment.converged) {
    return convergedPage(page, stage, attempts, "tier");
  }

  let previousResults = stage.results;
  for (
    let iteration = 0;
    iteration < args.maxMutationIterations;
    iteration += 1
  ) {
    const input = mutationAt(iteration, previousResults);
    stage = await runTiered(page, input, protocols);
    attempts.push(...stage.results);
    if (stage.assessment.converged) {
      return convergedPage(page, stage, attempts, "mutation");
    }
    previousResults = stage.results;
  }
  return arbitrate(page, attempts);
}

(async () => {
  const configured = validatedInputs();
  const pages = await parallel(
    configured.pages.map(page => () =>
      resolvePage(page, configured.protocols)
    )
  );
  return {
    schemaVersion: 1,
    pageCount: pages.length,
    convergedCount: pages.filter(page => page.status === "converged").length,
    arbitratedCount: pages.filter(page => page.status === "arbitrated").length,
    configuredNodeUpperBound: configured.upperBound,
    pages
  };
})();
