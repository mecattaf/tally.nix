"use strict";

const __flowOutcome = promise => Promise.resolve(promise).then(
  value => ({ ok: true, value }),
  error => ({ ok: false, error })
);

const __flowThenable = value =>
  value !== null &&
  (typeof value === "object" || typeof value === "function") &&
  typeof value.then === "function";

const __flowDescribe = value => {
  if (value === undefined) {
    return "undefined";
  }
  if (value === null) {
    return "null";
  }
  return `a ${typeof value}`;
};

const __flowNotThenable = (code, what, index, value, hint, location) => {
  const error = __flowError(
    "FlowCombinatorError",
    code,
    `${what} returned ${__flowDescribe(value)} instead of a promise; ${hint}`,
    { index },
    location
  );
  error.index = index;
  return error;
};

const __flowAggregate = (outcomes, location) => {
  const error = __flowError(
    "FlowAggregateError",
    "aggregate-failure",
    "one or more flow branches failed",
    { outcomes },
    location
  );
  error.outcomes = outcomes;
  return error;
};

globalThis.parallel = function parallel(thunks, options = {}) {
  const callLocation = __flowLocation();
  if (!Array.isArray(thunks) || thunks.some(thunk => typeof thunk !== "function")) {
    throw __flowError(
      "FlowCombinatorError",
      "parallel-invalid",
      "parallel() requires an array of thunks"
    );
  }
  if (
    options === null ||
    typeof options !== "object" ||
    Object.keys(options).some(key => key !== "settle") ||
    ("settle" in options && typeof options.settle !== "boolean")
  ) {
    throw __flowError(
      "FlowCombinatorError",
      "parallel-invalid",
      "parallel() options may contain only a boolean settle field"
    );
  }
  const invalid = [];
  const promises = thunks.map((thunk, index) => {
    let value;
    try {
      value = thunk();
    } catch (error) {
      return Promise.resolve({ ok: false, error });
    }
    if (!__flowThenable(value)) {
      const error = __flowNotThenable(
        "parallel-invalid",
        `parallel() thunk ${index}`,
        index,
        value,
        "a brace-bodied thunk such as () => { sh(...) } discards its node — " +
          "remove the braces or add an explicit return",
        callLocation
      );
      invalid.push(error);
      return Promise.resolve({ ok: false, error });
    }
    return __flowOutcome(value);
  });
  return Promise.all(promises).then(outcomes => {
    if (invalid.length) {
      throw invalid[0];
    }
    if (options && options.settle === true) {
      return outcomes;
    }
    if (outcomes.some(outcome => !outcome.ok)) {
      throw __flowAggregate(outcomes, callLocation);
    }
    return outcomes.map(outcome => outcome.value);
  });
};

globalThis.pipeline = function pipeline(items, ...rest) {
  const callLocation = __flowLocation();
  if (!Array.isArray(items)) {
    throw __flowError(
      "FlowCombinatorError",
      "pipeline-invalid",
      "pipeline() requires an item array"
    );
  }
  let options = {};
  if (
    rest.length &&
    typeof rest[rest.length - 1] === "object" &&
    rest[rest.length - 1] !== null &&
    typeof rest[rest.length - 1] !== "function"
  ) {
    options = rest.pop();
  }
  if (rest.some(stage => typeof stage !== "function")) {
    throw __flowError(
      "FlowCombinatorError",
      "pipeline-invalid",
      "pipeline stages must be functions"
    );
  }
  if (
    options === null ||
    typeof options !== "object" ||
    Object.keys(options).some(key => key !== "settle") ||
    ("settle" in options && typeof options.settle !== "boolean")
  ) {
    throw __flowError(
      "FlowCombinatorError",
      "pipeline-invalid",
      "pipeline() options may contain only a boolean settle field"
    );
  }
  const invalid = [];
  const chains = items.map((item, index) => {
    let chain = Promise.resolve(item);
    rest.forEach((stage, stageIndex) => {
      chain = chain.then(previous => {
        const value = stage(previous, item, index);
        if (!__flowThenable(value)) {
          const error = __flowNotThenable(
            "pipeline-invalid",
            `pipeline() stage ${stageIndex} for item ${index}`,
            index,
            value,
            "a brace-bodied stage such as (previous, item) => { sh(...) } discards its node — " +
              "remove the braces, add an explicit return, or declare the stage async",
            callLocation
          );
          error.stage = stageIndex;
          invalid.push(error);
          throw error;
        }
        return value;
      });
    });
    return __flowOutcome(chain);
  });
  return Promise.all(chains).then(outcomes => {
    if (invalid.length) {
      throw invalid[0];
    }
    if (options && options.settle === true) {
      return outcomes;
    }
    if (outcomes.some(outcome => !outcome.ok)) {
      throw __flowAggregate(outcomes, callLocation);
    }
    return outcomes.map(outcome => outcome.value);
  });
};

globalThis.attributed = function attributed(member, candidate) {
  const memberId = typeof member === "string" ? member : member && member.id;
  if (typeof memberId !== "string" || memberId.length === 0) {
    throw __flowError(
      "FlowDissentError",
      "attribution-invalid",
      "attributed() requires a member id"
    );
  }
  return { memberId, candidate };
};

globalThis.repairKey = function repairKey(member) {
  const memberId = typeof member === "string" ? member : member && member.id;
  if (typeof memberId !== "string" || memberId.length === 0) {
    throw __flowError(
      "FlowRepairError",
      "repair-member-invalid",
      "repairKey() requires a member id"
    );
  }
  return `${memberId}@1`;
};

globalThis.quorum = function quorum(declaration) {
  if (declaration === null || typeof declaration !== "object") {
    throw __flowError(
      "FlowQuorumError",
      "quorum-invalid",
      "quorum() requires a declaration object"
    );
  }
  const {
    results,
    minimumValid,
    requiredMembers,
    allowPartial = false
  } = declaration;
  if (
    !Array.isArray(results) ||
    !Array.isArray(requiredMembers) ||
    new Set(requiredMembers).size !== requiredMembers.length ||
    !Number.isInteger(minimumValid) ||
    minimumValid < 1 ||
    minimumValid > requiredMembers.length ||
    requiredMembers.some(member => typeof member !== "string" || member.length === 0) ||
    typeof allowPartial !== "boolean" ||
    Object.keys(declaration).some(
      key =>
        key !== "results" &&
        key !== "minimumValid" &&
        key !== "requiredMembers" &&
        key !== "allowPartial"
    )
  ) {
    throw __flowError(
      "FlowQuorumError",
      "quorum-invalid",
      "quorum() received an invalid declaration"
    );
  }
  const byMember = new Map();
  for (const entry of results) {
    const attribution =
      entry && typeof entry === "object" && typeof entry.memberId === "string"
        ? entry
        : null;
    const candidate = attribution ? attribution.candidate : entry;
    const outcome =
      candidate && typeof candidate === "object" && "ok" in candidate
        ? candidate
        : { ok: true, value: candidate };
    const value = outcome.ok ? outcome.value : undefined;
    const memberId = attribution
      ? attribution.memberId
      : value && (value.memberId || (value.selection && value.selection.memberId));
    if (typeof memberId === "string") {
      if (byMember.has(memberId)) {
        throw __flowError(
          "FlowQuorumError",
          "quorum-invalid",
          `quorum() received more than one result for ${memberId}`
        );
      }
      byMember.set(memberId, outcome);
    }
  }
  const valid = [];
  const invalid = [];
  const missing = [];
  for (const memberId of requiredMembers) {
    const outcome = byMember.get(memberId);
    if (!outcome) {
      missing.push({ memberId, status: "missing" });
    } else if (
      !outcome.ok ||
      !outcome.value ||
      outcome.value.verdict !== "pass" ||
      outcome.value.error
    ) {
      invalid.push({ memberId, status: "invalid", outcome });
    } else {
      valid.push({ memberId, result: outcome.value });
    }
  }
  const summary = {
    requiredMembers: requiredMembers.slice(),
    minimumValid,
    allowPartial: allowPartial === true,
    valid,
    invalid,
    missing
  };
  if (
    valid.length < minimumValid ||
    (valid.length < requiredMembers.length && allowPartial !== true)
  ) {
    const error = __flowError(
      "FlowQuorumError",
      "quorum-not-met",
      `quorum not met: ${valid.length} valid of ${requiredMembers.length} required`,
      summary
    );
    error.quorum = summary;
    throw error;
  }
  return summary;
};

globalThis.dissent = function dissent(declaration) {
  if (declaration === null || typeof declaration !== "object") {
    throw __flowError(
      "FlowDissentError",
      "dissent-invalid",
      "dissent() requires a declaration object"
    );
  }
  const { conclusions, excluded = [] } = declaration;
  if (!Array.isArray(conclusions) || !Array.isArray(excluded)) {
    throw __flowError(
      "FlowDissentError",
      "dissent-invalid",
      "dissent() requires conclusions and excluded arrays"
    );
  }
  if (
    Object.keys(declaration).some(
      key => key !== "conclusions" && key !== "excluded"
    )
  ) {
    throw __flowError(
      "FlowDissentError",
      "dissent-invalid",
      "dissent() declaration has an unknown field"
    );
  }
  const normalized = conclusions.map(conclusion => {
    if (
      !conclusion ||
      !Object.prototype.hasOwnProperty.call(conclusion, "conclusion") ||
      !Array.isArray(conclusion.support) ||
      !Array.isArray(conclusion.conflict) ||
      Object.keys(conclusion).some(
        key =>
          key !== "conclusion" && key !== "support" && key !== "conflict"
      ) ||
      conclusion.support.length === 0 ||
      conclusion.support.some(value => typeof value !== "string") ||
      conclusion.conflict.some(value => typeof value !== "string") ||
      new Set(conclusion.support).size !== conclusion.support.length ||
      new Set(conclusion.conflict).size !== conclusion.conflict.length ||
      conclusion.support.some(value => conclusion.conflict.includes(value))
    ) {
      throw __flowError(
        "FlowDissentError",
        "dissent-attribution-missing",
        "every conclusion must carry support and conflict member-id arrays"
      );
    }
    return {
      conclusion: conclusion.conclusion,
      support: conclusion.support.slice(),
      conflict: conclusion.conflict.slice()
    };
  });
  const excludedRows = excluded.map(row => {
    if (
      !row ||
      typeof row.memberId !== "string" ||
      typeof row.reason !== "string" ||
      Object.keys(row).some(key => key !== "memberId" && key !== "reason")
    ) {
      throw __flowError(
        "FlowDissentError",
        "dissent-attribution-missing",
        "every excluded row must carry memberId and reason"
      );
    }
    return { memberId: row.memberId, reason: row.reason };
  });
  return { conclusions: normalized, excluded: excludedRows };
};
