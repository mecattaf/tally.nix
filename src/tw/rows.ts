// tally — durable-row admission + row (de)serialization (IMPLEMENTATION-PLAN M1.3 `rows.ts`).
//
// The veneer's admission gate and the mapping between a tally job's identity and a taskwarrior
// row. Two disciplines are enforced here:
//
//  1. Durable-row admission (SPEC appendix; CLI-SURFACE §1.1a): a row is written ONLY for
//     autonomous/batch/queued units needing cross-source urgency ranking OR crash-survival — one
//     row per durable job, one standing row per drain. A unit spawned live by a running
//     orchestrator earns NO row (`task_uuid: null`); its record is the JSONL + witness.
//
//  2. Veneer discipline (SPEC "thin durable veneer"): NO high-frequency machine state — heartbeats,
//     leases, evidence — is ever written to TW. `assertVeneerClean` is the guard the veneer applies
//     to every outgoing row so a machine-state field can never leak into a row (the veneer-
//     discipline test asserts this).
//
// Priority maps to native TW `priority` (H|M|L) — the urgency engine input (PS#1a) — while the
// original tally priority is also mirrored into the `priority_class` UDA so a round-trip is lossless.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import {
  admitsDurableRow,
  twPriority,
  TALLY_UDA_NAMES,
  type AdmissionInput,
  type TaskRow,
  type TaskStatus,
  type Trust,
} from "../contracts/task";
import type { EnqueueParams, JobState, Priority, Source } from "../contracts/job";
import type { AgentKind } from "../contracts/agent";
import { ValidationError } from "../contracts/errors";

/**
 * Fields that describe high-frequency MACHINE STATE and MUST NEVER be written to a TW row (SPEC
 * veneer discipline). A row that carries any of these is a veneer violation — {@link assertVeneerClean}
 * throws. These are the shapes the witness/journald/lease surfaces own, never TaskChampion.
 */
export const FORBIDDEN_ROW_FIELDS: readonly string[] = [
  "heartbeat",
  "gpu_seconds",
  "wall_clock",
  "artifact_content_hash",
  "artifact_hash",
  "exit_code",
  "verdict",
  "prev_hash",
  "hash",
  "seq",
  "witness_lsn",
  "lease_generation",
  "lease_holder",
  "evidence",
  "charge",
];

/**
 * The durable-row admission decision (SPEC appendix; CLI-SURFACE §1.1a). Thin wrapper over the
 * contract predicate so `rows.ts` is the one place the veneer asks "does this unit earn a row?".
 * Returns true ⇒ write a row; false ⇒ `task_uuid: null`.
 */
export function admits(input: AdmissionInput): boolean {
  return admitsDurableRow(input);
}

/**
 * Derive the admission input for a Seam-A enqueue. A `source` of `orchestrator` marks a
 * live-orchestrator-spawned unit (no row) UNLESS the caller explicitly declares durability via
 * `overrides` (e.g. a durable drain standing-row). Every non-orchestrator source is autonomous/
 * batch/queued (the OCR firehose is `r2`, gh intake is `gh`, timers are `calendar`, a human is
 * `manual`) and therefore earns a row.
 */
export function admissionForEnqueue(
  params: Pick<EnqueueParams, "source">,
  overrides: Partial<AdmissionInput> = {},
): AdmissionInput {
  const liveOrchestratorSpawned = params.source === "orchestrator";
  const base: AdmissionInput = {
    source: params.source,
    liveOrchestratorSpawned,
    autonomous: !liveOrchestratorSpawned,
    crashSurvivable: !liveOrchestratorSpawned,
    needsCrossSourceUrgency: !liveOrchestratorSpawned,
  };
  return { ...base, ...overrides };
}

/** The tally-owned fields that seed a durable row at enqueue time (before dispatch). */
export interface RowSeed {
  uuid: string;
  description: string;
  priority: Priority;
  source: Source;
  kind: AgentKind;
  cwd?: string;
  worktree?: string;
  pool?: string;
  model_class?: string;
  dedup_key?: string;
  session_ref?: string | null;
  lease_epoch?: number;
  attempt?: number;
  /** The verbatim leaf argv, JSON-encoded (static job identity — recovery re-reads it verbatim). */
  argv_json?: string;
  /** The declared evidence checks, JSON-encoded (static gates — recovery re-arms them). */
  evidence_json?: string;
  /** Optional entry timestamp override (compact taskwarrior datetime); defaults to now via `now`. */
  entry?: string;
}

/**
 * Normalize a datetime string to the taskwarrior COMPACT form `YYYYMMDDTHHMMSSZ` (UTC, no fractional
 * seconds) — the canonical form `task import`/`task export` round-trips. Accepts an ISO-8601 string
 * (with or without milliseconds) OR an already-compact string (returned unchanged). A fractional-second
 * ISO value fed to real taskwarrior 3 on a non-UTC host mis-parses the instant, so we strip it here.
 */
export function toTwDatetime(value: string): string {
  // Already compact (YYYYMMDDTHHMMSSZ)?
  if (/^\d{8}T\d{6}Z$/.test(value)) return value;
  const d = new Date(value);
  if (Number.isNaN(d.getTime())) return value; // not a datetime — leave it (defensive)
  const iso = d.toISOString(); // always UTC, ms-bearing: 2026-07-09T12:00:00.512Z
  // Drop dashes/colons and the fractional-second part → 20260709T120000Z.
  return iso.replace(/[-:]/g, "").replace(/\.\d+Z$/, "Z");
}

/**
 * Build a fresh, admission-passing durable {@link TaskRow} from a seed. Status starts `pending`;
 * `trust` is left UNSET at enqueue (it is written `unreviewed` only at COMPLETION — SPEC "The trust
 * review UDA"). The result is run through {@link assertVeneerClean} so a malformed seed can never
 * produce a machine-state-carrying row.
 */
export function buildRow(seed: RowSeed, now: () => string): TaskRow {
  if (seed.cwd !== undefined && seed.worktree !== undefined) {
    throw new ValidationError("a row carries cwd XOR worktree, not both", "cwd/worktree");
  }
  const entry = seed.entry ?? now();
  const row: TaskRow = {
    uuid: seed.uuid,
    description: seed.description,
    status: "pending",
    priority: twPriority(seed.priority),
    priority_class: seed.priority,
    source: seed.source,
    agent: seed.kind,
    labor_class: "fresh",
    entry,
    modified: entry,
  };
  if (seed.cwd !== undefined) row.cwd = seed.cwd;
  if (seed.worktree !== undefined) row.worktree = seed.worktree;
  if (seed.pool !== undefined) row.pool = seed.pool;
  if (seed.model_class !== undefined) row.model_class = seed.model_class;
  if (seed.dedup_key !== undefined) row.dedup_key = seed.dedup_key;
  if (seed.session_ref !== undefined && seed.session_ref !== null) row.session_ref = seed.session_ref;
  if (seed.lease_epoch !== undefined) row.lease_epoch = seed.lease_epoch;
  if (seed.attempt !== undefined) row.attempt = seed.attempt;
  if (seed.argv_json !== undefined) row.argv_json = seed.argv_json;
  if (seed.evidence_json !== undefined) row.evidence_json = seed.evidence_json;
  assertVeneerClean(row);
  return row;
}

/**
 * The mapping from a tally job lifecycle state to the taskwarrior row `status`. Only the terminal
 * transitions move the row: a job stays `pending` through dispatch/started/heartbeat (machine state
 * that never leaks), flips to `completed` on success, and `deleted` on a cancel. A `failed` job's
 * row stays `pending` so recover() re-presents it (undeleted-row = re-dispatch, PS#9 invariant 4).
 */
export function statusForJobState(state: JobState): TaskStatus {
  switch (state) {
    case "completed":
      return "completed";
    default:
      return "pending";
  }
}

/**
 * Apply a completion to a durable row: flip `status` to `completed`, stamp `end`, write the labor
 * class, and — the trust-review discipline — write `trust:unreviewed` (SPEC "The trust review UDA":
 * written `unreviewed` at completion, flipped only by review/recall, NEVER blocks future work). The
 * returned row is a NEW object (the input is never mutated) and is veneer-clean.
 */
export function completeRow(
  row: TaskRow,
  opts: { laborClass?: TaskRow["labor_class"]; trust?: Trust } = {},
  now: () => string = () => new Date().toISOString(),
): TaskRow {
  const end = now();
  const next: TaskRow = {
    ...row,
    status: "completed",
    end,
    modified: end,
    labor_class: opts.laborClass ?? row.labor_class ?? "fresh",
    trust: opts.trust ?? "unreviewed",
  };
  assertVeneerClean(next);
  return next;
}

/** Flip a row's `trust` field (the review/recall path). Never touches any other field. */
export function setTrust(row: TaskRow, trust: Trust, now: () => string = () => new Date().toISOString()): TaskRow {
  const modified = now();
  return { ...row, trust, modified };
}

/**
 * Mark a durable row `deleted` (the cancel path). Recovery treats a deleted row as done work — it
 * is NOT re-presented (only an undeleted row is re-dispatched, PS#9 invariant 4).
 */
export function cancelRow(row: TaskRow, now: () => string = () => new Date().toISOString()): TaskRow {
  const modified = now();
  return { ...row, status: "deleted", end: modified, modified };
}

/**
 * The veneer guard (SPEC "thin durable veneer"): throw if a row carries any high-frequency
 * machine-state field. Every outgoing row passes through here before `task import`, so a
 * heartbeat/lease/evidence-shaped write can never reach TaskChampion. This is the assertion the
 * veneer-discipline test exercises.
 */
export function assertVeneerClean(row: TaskRow): void {
  for (const field of FORBIDDEN_ROW_FIELDS) {
    if (Object.prototype.hasOwnProperty.call(row, field) && row[field] !== undefined) {
      throw new ValidationError(
        `veneer violation: row ${row.uuid} carries forbidden machine-state field '${field}' ` +
          `(heartbeats/leases/evidence never touch TaskChampion — SPEC veneer discipline)`,
        field,
      );
    }
  }
}

/**
 * The set of tally-managed attribute names on a row (native `priority` plus every UDA). Used by the
 * op-log shadow derivation to diff only the fields the veneer owns, and available for a
 * merge-not-clobber overlay that preserves foreign attributes.
 */
export const MANAGED_ROW_FIELDS: readonly string[] = ["priority", ...TALLY_UDA_NAMES];

/**
 * Overlay tally-managed fields from `update` onto `base` WITHOUT clobbering foreign attributes the
 * veneer does not own (merge-not-clobber discipline). Only keys in {@link MANAGED_ROW_FIELDS} plus
 * the mutable native columns (`status`, `end`, `modified`, `description`) are taken from `update`;
 * everything else is preserved from `base`. The result is veneer-clean.
 */
export function overlayManaged(base: TaskRow, update: Partial<TaskRow>): TaskRow {
  const merged: TaskRow = { ...base };
  const mutableNative = ["status", "end", "modified", "description"] as const;
  for (const key of [...MANAGED_ROW_FIELDS, ...mutableNative]) {
    if (Object.prototype.hasOwnProperty.call(update, key) && update[key] !== undefined) {
      merged[key] = update[key];
    }
  }
  assertVeneerClean(merged);
  return merged;
}
