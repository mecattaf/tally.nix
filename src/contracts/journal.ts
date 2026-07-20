// tally — journald TALLY_* event schema (SPEC "journald TALLY_* event schema", FS§4 + PS#11;
// IMPLEMENTATION-PLAN §3 Journald, M1.4).
//
// journald is observability, NOT load-bearing memory — the witness ledger is emitted from these
// fields but is a separate artifact. Under the Bun flip the fields ride a single-line JSON MESSAGE
// captured by `StandardOutput=journal` / `SyslogIdentifier=tally` (SPEC "Emission path"); the
// reader parses them back out. This file owns the field matrix, the event vocabulary, and the
// golden-tested `AgentKind → TALLY_AGENT` short-vocabulary map both the writer and reader use.

import type { AgentKind } from "./agent";
import type { LaborClass, Priority, Source } from "./job";

/**
 * The journald TALLY_EVENT vocabulary (SPEC journald table). The `job.*` delta events mirror these
 * VERBATIM — one vocabulary, not a second source of truth (CLI-SURFACE §2.3).
 */
export type TallyEvent =
  | "enqueued"
  | "dispatched"
  | "started"
  | "heartbeat"
  | "preempted"
  | "resumed"
  | "completed"
  | "failed"
  | "evidence_pass"
  | "evidence_fail"
  | "witness_emitted";

export const TALLY_EVENTS = [
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
] as const satisfies readonly TallyEvent[];

/**
 * The short TALLY_AGENT vocabulary (SPEC journald table) — `pi | cc | shell | <worker>`, NOT the
 * `AgentKind` spelling. A raw worker label passes through as `<worker>`.
 */
export type TallyAgent = "pi" | "cc" | "shell" | (string & {});

/**
 * The golden-tested `AgentKind → TALLY_AGENT` map (IMPLEMENTATION-PLAN §3 Journald / risk 10).
 * Both `emit.ts` (writer) and the `query log`/standup reader/join go through this ONE function, so
 * the journald table and the reader never disagree on the spelling. `claude-code → "cc"`.
 */
export function tallyAgent(kind: AgentKind): TallyAgent {
  switch (kind) {
    case "claude-code":
      return "cc";
    case "pi":
      return "pi";
    case "shell":
      return "shell";
  }
}

/**
 * The inverse map for the reader/join: a short TALLY_AGENT label back to an `AgentKind` where it is
 * one of the three; a raw `<worker>` label has no `AgentKind` and returns null.
 */
export function agentKindFromTally(label: string): AgentKind | null {
  switch (label) {
    case "cc":
      return "claude-code";
    case "pi":
      return "pi";
    case "shell":
      return "shell";
    default:
      return null;
  }
}

/**
 * The structured TALLY_* fields of one journald entry (SPEC journald table). Under stdout capture
 * these ride as a single-line JSON MESSAGE payload; `SYSLOG_IDENTIFIER=tally` is fixed by the unit.
 * Optional fields are the ones required only at certain stages (see `TALLY_FIELD_MATRIX`).
 */
export interface TallyFields {
  /** `tally` (fixed). Set by the unit's SyslogIdentifier; carried here for the reader. */
  SYSLOG_IDENTIFIER: "tally";
  TALLY_EVENT: TallyEvent;
  TALLY_TASK_UUID: string;
  TALLY_CLASS: Priority;
  TALLY_SOURCE: Source;
  TALLY_AGENT?: TallyAgent;
  TALLY_SESSION_REF?: string;
  TALLY_UNIT?: string;
  TALLY_EXIT_CODE?: number;
  TALLY_GPU_SECONDS?: number;
  TALLY_ARTIFACT_HASH?: string;
  TALLY_EVIDENCE?: string;
  TALLY_ATTEMPT?: number;
  TALLY_LEASE_EPOCH?: number;
  TALLY_LABOR_CLASS?: LaborClass;
  /** One human-readable line. */
  MESSAGE: string;
}

/** The stage at which a field becomes required (SPEC journald table "Required" column). */
export type FieldRequirement =
  | "always"
  | "at-dispatch+"
  | "at-start+"
  | "at-completed"
  | "at-completed-or-failed"
  | "at-evidence"
  | "when-agent-run";

/**
 * The field → required-at-stage matrix (SPEC journald table). Golden-tested for completeness so no
 * field is silently dropped. The writer asserts every `always` field is present on every event and
 * every stage-gated field is present at its stage.
 */
export const TALLY_FIELD_MATRIX = {
  SYSLOG_IDENTIFIER: "always",
  TALLY_EVENT: "always",
  TALLY_TASK_UUID: "always",
  TALLY_CLASS: "always",
  TALLY_SOURCE: "always",
  MESSAGE: "always",
  TALLY_AGENT: "at-dispatch+",
  TALLY_ATTEMPT: "at-dispatch+",
  TALLY_LEASE_EPOCH: "at-dispatch+",
  TALLY_UNIT: "at-start+",
  TALLY_SESSION_REF: "when-agent-run",
  TALLY_EXIT_CODE: "at-completed-or-failed",
  TALLY_GPU_SECONDS: "at-completed-or-failed",
  TALLY_LABOR_CLASS: "at-completed-or-failed",
  TALLY_ARTIFACT_HASH: "at-completed",
  TALLY_EVIDENCE: "at-evidence",
} as const satisfies Record<keyof TallyFields, FieldRequirement>;

/** The always-required field names — the writer asserts these on every event. */
export const ALWAYS_FIELDS = (Object.keys(TALLY_FIELD_MATRIX) as (keyof TallyFields)[]).filter(
  (k) => TALLY_FIELD_MATRIX[k] === "always",
);
