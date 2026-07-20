// tally — journald TALLY_* emission (writer half of M1.4; SPEC "journald TALLY_* event schema",
// FS§4 + PS#11; IMPLEMENTATION-PLAN §3 Journald).
//
// journald is observability, NOT load-bearing memory — the witness ledger is emitted from these
// same fields but is a separate artifact (SPEC). Under the ruled emission path
// (`StandardOutput=journal` + `SyslogIdentifier=tally` in the daemon unit, DECISIONS jul9) the
// daemon writes exactly ONE structured line per event to stdout, which journald captures. Because
// Bun cannot open the AF_UNIX SOCK_DGRAM socket the native journal protocol needs (SPEC "Emission
// path"), the structured TALLY_* fields ride as a single-line JSON `MESSAGE` payload; the reader
// (`reader.ts`) parses them back out.
//
// This module owns: the per-event field-matrix enforcement, the single-line (no embedded newline)
// guarantee, the `AgentKind → TALLY_AGENT` short-vocabulary mapping (via the golden-tested
// `tallyAgent` from contracts), and the human-readable MESSAGE synthesis. It performs NO subprocess
// access — emission is a plain stdout write — so it takes no `Exec`. The stage-gated field matrix
// is validated at emit time so a caller that omits a required field fails loudly rather than
// silently dropping proof-bearing data.

import {
  TALLY_EVENTS,
  TALLY_FIELD_MATRIX,
  tallyAgent,
  ValidationError,
  type AgentKind,
  type FieldRequirement,
  type LaborClass,
  type Priority,
  type Source,
  type TallyAgent,
  type TallyEvent,
  type TallyFields,
} from "../contracts/index.ts";

/**
 * The caller-facing event input. This is the structured shape the jobs engine (M2.2) hands the
 * emitter for one lifecycle transition. `agent_kind` is an `AgentKind` (`claude-code` etc.); the
 * emitter maps it to the short `TALLY_AGENT` vocabulary (`cc` etc.) via the contracts `tallyAgent`
 * function so the writer and reader never disagree on the spelling. A raw worker label (a `pool`
 * worker with no `AgentKind`) may be passed as `agent_label` instead.
 *
 * `SYSLOG_IDENTIFIER` and `MESSAGE` are NOT part of the input — the identifier is fixed and the
 * message is synthesized (or overridden via `message`).
 */
export interface EmitEvent {
  event: TallyEvent;
  task_uuid: string;
  class: Priority;
  source: Source;
  /** One of the three agent kinds; mapped to the short vocabulary. Mutually exclusive with `agent_label`. */
  agent_kind?: AgentKind;
  /** A raw worker label that has no `AgentKind` (`<worker>` in the SPEC table). */
  agent_label?: string;
  session_ref?: string;
  unit?: string;
  exit_code?: number;
  gpu_seconds?: number;
  artifact_hash?: string;
  evidence?: string;
  attempt?: number;
  lease_epoch?: number;
  labor_class?: LaborClass;
  /** Override the synthesized human-readable line. */
  message?: string;
}

/**
 * A sink for one rendered journald line. Production writes to stdout (captured by
 * `StandardOutput=journal`); tests inject a collector. The line NEVER contains an embedded newline;
 * the sink is responsible for the trailing terminator (the default stdout sink appends `\n`).
 */
export type JournalSink = (line: string) => void;

/** The default sink: one line to stdout, LF-terminated, captured by journald. */
export const stdoutSink: JournalSink = (line: string): void => {
  // A single write of the line plus its LF terminator — one journald record per line.
  process.stdout.write(line + "\n");
};

/**
 * Resolve the short `TALLY_AGENT` label for an event, or undefined when neither an `AgentKind` nor a
 * raw worker label is supplied (the field is optional pre-dispatch). `agent_kind` and `agent_label`
 * are mutually exclusive; supplying both is a caller bug.
 */
function resolveAgent(ev: EmitEvent): TallyAgent | undefined {
  if (ev.agent_kind !== undefined && ev.agent_label !== undefined) {
    throw new ValidationError(
      "emit: agent_kind and agent_label are mutually exclusive",
      "agent",
    );
  }
  if (ev.agent_kind !== undefined) return tallyAgent(ev.agent_kind);
  if (ev.agent_label !== undefined) {
    if (ev.agent_label.length === 0) {
      throw new ValidationError("emit: agent_label must be non-empty", "agent_label");
    }
    return ev.agent_label;
  }
  return undefined;
}

/**
 * Synthesize the human-readable MESSAGE line when the caller supplies none. One terse line naming
 * the event and its anchor UUID, with the most salient stage detail appended.
 */
function synthesizeMessage(ev: EmitEvent): string {
  const parts = [ev.event, ev.task_uuid];
  switch (ev.event) {
    case "completed":
    case "failed":
      if (ev.exit_code !== undefined) parts.push(`exit=${ev.exit_code}`);
      if (ev.gpu_seconds !== undefined) parts.push(`gpu=${ev.gpu_seconds}s`);
      break;
    case "evidence_pass":
    case "evidence_fail":
      if (ev.evidence !== undefined) parts.push(ev.evidence);
      break;
    case "dispatched":
    case "started":
      if (ev.unit !== undefined) parts.push(ev.unit);
      if (ev.attempt !== undefined) parts.push(`attempt=${ev.attempt}`);
      break;
    default:
      break;
  }
  return parts.join(" ");
}

/**
 * Build the fully-typed `TallyFields` record for an event, mapping the caller input to the wire
 * field names. Optional fields are set ONLY when present (honoring `exactOptionalPropertyTypes`).
 */
function buildFields(ev: EmitEvent): TallyFields {
  const agent = resolveAgent(ev);
  const fields: TallyFields = {
    SYSLOG_IDENTIFIER: "tally",
    TALLY_EVENT: ev.event,
    TALLY_TASK_UUID: ev.task_uuid,
    TALLY_CLASS: ev.class,
    TALLY_SOURCE: ev.source,
    MESSAGE: ev.message ?? synthesizeMessage(ev),
  };
  if (agent !== undefined) fields.TALLY_AGENT = agent;
  if (ev.session_ref !== undefined) fields.TALLY_SESSION_REF = ev.session_ref;
  if (ev.unit !== undefined) fields.TALLY_UNIT = ev.unit;
  if (ev.exit_code !== undefined) fields.TALLY_EXIT_CODE = ev.exit_code;
  if (ev.gpu_seconds !== undefined) fields.TALLY_GPU_SECONDS = ev.gpu_seconds;
  if (ev.artifact_hash !== undefined) fields.TALLY_ARTIFACT_HASH = ev.artifact_hash;
  if (ev.evidence !== undefined) fields.TALLY_EVIDENCE = ev.evidence;
  if (ev.attempt !== undefined) fields.TALLY_ATTEMPT = ev.attempt;
  if (ev.lease_epoch !== undefined) fields.TALLY_LEASE_EPOCH = ev.lease_epoch;
  if (ev.labor_class !== undefined) fields.TALLY_LABOR_CLASS = ev.labor_class;
  return fields;
}

/**
 * The stages, in lifecycle order, at which each `FieldRequirement` becomes mandatory. A field
 * required "at-dispatch+" must be present on `dispatched` and every event after it. This ordering
 * mirrors the journald table's "Required" column (SPEC).
 */
const STAGE_ORDER: TallyEvent[] = [
  "enqueued",
  "dispatched",
  "started",
  "heartbeat",
  "preempted",
  "resumed",
  "completed",
  "failed",
  "evidence_pass",
  "evidence_fail",
  "witness_emitted",
];

/** The rank of an event in the lifecycle ordering (for the "+" stage-gated requirements). */
function stageRank(event: TallyEvent): number {
  const idx = STAGE_ORDER.indexOf(event);
  return idx === -1 ? Number.MAX_SAFE_INTEGER : idx;
}

const DISPATCH_RANK = stageRank("dispatched");
const START_RANK = stageRank("started");

/** The set of events that count as "completed/failed" for the at-completed-or-failed requirement. */
const COMPLETED_OR_FAILED: ReadonlySet<TallyEvent> = new Set<TallyEvent>(["completed", "failed"]);
/** The set of events that count as "evidence" for the at-evidence requirement. */
const EVIDENCE_EVENTS: ReadonlySet<TallyEvent> = new Set<TallyEvent>([
  "evidence_pass",
  "evidence_fail",
]);

/**
 * Decide whether a field with the given requirement is mandatory on the given event. Non-mandatory
 * fields may still be present (they are optional-at-earlier-stages); this only governs what MUST be
 * there so a missing proof-bearing field fails loudly.
 *
 * `when-agent-run` (TALLY_SESSION_REF) is genuinely conditional on whether the unit is an agent run
 * (shell runs have no session_ref), so it is NEVER treated as unconditionally mandatory here — the
 * caller supplies it when an agent produced the event; its presence is validated by shape, not by
 * stage.
 */
function isRequiredAt(requirement: FieldRequirement, event: TallyEvent): boolean {
  switch (requirement) {
    case "always":
      return true;
    case "at-dispatch+":
      return stageRank(event) >= DISPATCH_RANK;
    case "at-start+":
      return stageRank(event) >= START_RANK;
    case "at-completed":
      return event === "completed";
    case "at-completed-or-failed":
      return COMPLETED_OR_FAILED.has(event);
    case "at-evidence":
      return EVIDENCE_EVENTS.has(event);
    case "when-agent-run":
      // Conditional on run kind, not on stage — validated by presence when an agent ran, never
      // forced by the stage matrix.
      return false;
  }
}

/**
 * Validate a built field record against the SPEC field matrix for its event. Throws a
 * `ValidationError` naming the first missing mandatory field. This is the "no field silently
 * dropped" guarantee the writer owes the witness (which is derived from these fields).
 */
export function validateFields(fields: TallyFields): void {
  const event = fields.TALLY_EVENT;
  if (!TALLY_EVENTS.includes(event)) {
    throw new ValidationError(`emit: unknown TALLY_EVENT '${String(event)}'`, "TALLY_EVENT");
  }
  for (const key of Object.keys(TALLY_FIELD_MATRIX) as (keyof TallyFields)[]) {
    const requirement = TALLY_FIELD_MATRIX[key];
    if (!isRequiredAt(requirement, event)) continue;
    const value = fields[key];
    if (value === undefined || value === null || value === "") {
      throw new ValidationError(
        `emit: event '${event}' requires field ${key} (required-at: ${requirement})`,
        key,
      );
    }
  }
}

/**
 * Render a validated field record to its single journald line: a single-line JSON object. The line
 * is guaranteed to contain NO embedded newline — `JSON.stringify` escapes any newline inside a
 * string value as `\n`, and the object has no literal newlines of its own — but we assert it defensively
 * (a newline would split one event into two journald records and corrupt the reader's round-trip).
 */
export function renderLine(fields: TallyFields): string {
  const line = JSON.stringify(fields);
  if (line.includes("\n") || line.includes("\r")) {
    // Unreachable via JSON.stringify (control chars are escaped), but the single-line guarantee is
    // load-bearing for the reader, so we fail loudly rather than emit a split record.
    throw new ValidationError("emit: rendered line contains an embedded newline", "MESSAGE");
  }
  return line;
}

/**
 * The journald emitter. Holds the injected sink; `emit()` validates the field matrix, renders the
 * single line, and writes it. One instance per daemon; the production sink is `stdoutSink`.
 */
export class JournalEmitter {
  private readonly sink: JournalSink;

  constructor(sink: JournalSink = stdoutSink) {
    this.sink = sink;
  }

  /**
   * Emit one event. Builds the field record, enforces the required-at matrix, renders the
   * single-line JSON payload, and writes it through the sink. Returns the rendered line (also useful
   * for tests and for the witness derivation that reads the same fields).
   */
  emit(ev: EmitEvent): string {
    const fields = buildFields(ev);
    validateFields(fields);
    const line = renderLine(fields);
    this.sink(line);
    return line;
  }

  /**
   * Emit a pre-built `TallyFields` record directly (e.g. when a caller already assembled the wire
   * fields). Still validated + single-line-guaranteed.
   */
  emitFields(fields: TallyFields): string {
    validateFields(fields);
    const line = renderLine(fields);
    this.sink(line);
    return line;
  }
}

/** Convenience: build the fields for an event without emitting (for callers deriving the witness). */
export function toFields(ev: EmitEvent): TallyFields {
  const fields = buildFields(ev);
  validateFields(fields);
  return fields;
}
