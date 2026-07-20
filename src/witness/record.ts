// tally — witness record validation + projection/aggregation helpers (SPEC "Record schema";
// IMPLEMENTATION-PLAN M1.2). Hand-rolled narrowing (no zod — keep the compile small, plan rule).
//
// `parseRecord` turns a raw parsed JSONL object into a typed `WitnessRecord`, rejecting a
// structurally-invalid line (used by `verify.ts` and by the ledger boot-scan to distinguish a torn
// trailing line from a real record). The projection + canonical-GPU-seconds rules re-export the
// contract helpers and add the aggregation the SPEC's "excluded from canonical GPU-seconds" rule
// requires.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import {
  countsTowardCanonicalGpuSeconds,
  LABOR_CLASSES,
  toProjection,
  VERDICTS,
  type LaborClass,
  type Verdict,
  type WitnessProjection,
  type WitnessRecord,
} from "../contracts/index";

export { toProjection, countsTowardCanonicalGpuSeconds };
export type { WitnessProjection };

/** The outcome of validating one raw ledger object. */
export type ParseResult =
  | { ok: true; record: WitnessRecord }
  | { ok: false; reason: string };

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function isStringOrNull(v: unknown): v is string | null {
  return v === null || typeof v === "string";
}

/**
 * Validate a raw parsed object as a canonical `WitnessRecord`. Returns a discriminated result
 * carrying the exact failing reason (surfaced by `verify.ts`). This is deliberately strict on the
 * chain-load-bearing fields (`seq`, `prev_hash`, `hash`, the enums) and tolerant of the open
 * `verdict`/`pool` string spaces (the SPEC marks `verdict` open-ended, `pool` an open string).
 */
export function parseRecord(raw: unknown): ParseResult {
  if (!isObject(raw)) return { ok: false, reason: "line is not a JSON object" };

  // Chain fields — load-bearing, strict.
  if (typeof raw.seq !== "number" || !Number.isInteger(raw.seq) || raw.seq < 1) {
    return { ok: false, reason: "seq missing or not a positive integer" };
  }
  if (typeof raw.prev_hash !== "string" || !raw.prev_hash.startsWith("sha256:")) {
    return { ok: false, reason: "prev_hash missing or not a sha256: hash" };
  }
  if (typeof raw.hash !== "string" || !raw.hash.startsWith("sha256:")) {
    return { ok: false, reason: "hash missing or not a sha256: hash" };
  }

  // Anchor + timing.
  if (!isStringOrNull(raw.task_uuid)) {
    return { ok: false, reason: "task_uuid must be string or null" };
  }
  if (typeof raw.transition_timestamp !== "string") {
    return { ok: false, reason: "transition_timestamp missing or not a string" };
  }

  // Verdict (open-ended, but must be a string).
  if (typeof raw.verdict !== "string") {
    return { ok: false, reason: "verdict missing or not a string" };
  }

  if (typeof raw.exit_code !== "number" || !Number.isInteger(raw.exit_code)) {
    return { ok: false, reason: "exit_code missing or not an integer" };
  }
  if (!isStringOrNull(raw.artifact_content_hash)) {
    return { ok: false, reason: "artifact_content_hash must be string or null" };
  }
  if (raw.gpu_seconds !== null && typeof raw.gpu_seconds !== "number") {
    return { ok: false, reason: "gpu_seconds must be number or null" };
  }
  if (typeof raw.wall_clock !== "number") {
    return { ok: false, reason: "wall_clock missing or not a number" };
  }
  if (typeof raw.attempt !== "number" || !Number.isInteger(raw.attempt)) {
    return { ok: false, reason: "attempt missing or not an integer" };
  }
  if (typeof raw.lease_epoch !== "number" || !Number.isInteger(raw.lease_epoch)) {
    return { ok: false, reason: "lease_epoch missing or not an integer" };
  }
  if (!isStringOrNull(raw.dedup_key)) {
    return { ok: false, reason: "dedup_key must be string or null" };
  }
  if (typeof raw.labor_class !== "string" || !(LABOR_CLASSES as readonly string[]).includes(raw.labor_class)) {
    return { ok: false, reason: "labor_class not one of fresh|recovered|reused" };
  }
  if ("trace_ref" in raw && raw.trace_ref !== undefined && !isStringOrNull(raw.trace_ref)) {
    return { ok: false, reason: "trace_ref must be string or null when present" };
  }
  if (!isStringOrNull(raw.pool)) {
    return { ok: false, reason: "pool must be string or null" };
  }
  // charge: null OR {unit:string, amount:number, class:string}
  if (raw.charge !== null) {
    if (!isObject(raw.charge)) {
      return { ok: false, reason: "charge must be an object or null" };
    }
    if (typeof raw.charge.unit !== "string") {
      return { ok: false, reason: "charge.unit missing or not a string" };
    }
    if (typeof raw.charge.amount !== "number") {
      return { ok: false, reason: "charge.amount missing or not a number" };
    }
    if (typeof raw.charge.class !== "string") {
      return { ok: false, reason: "charge.class missing or not a string" };
    }
  }
  if (!isStringOrNull(raw.model)) {
    return { ok: false, reason: "model must be string or null" };
  }

  // All checks passed — the object is shape-valid. It is safe to treat the raw object AS the record
  // (its field set is a superset check above and the enums are narrowed); casting through unknown.
  return { ok: true, record: raw as unknown as WitnessRecord };
}

/**
 * Sum canonical GPU-seconds over a set of records: only `labor_class:fresh` lines whose verdict is
 * not `clean-exit-no-artifact`, and whose `gpu_seconds` is non-null, contribute (SPEC "Record
 * schema"; `countsTowardCanonicalGpuSeconds`). `reused`/`recovered` lines and gate-fails are
 * excluded. This is the aggregation `query standup` and the meter build on.
 */
export function canonicalGpuSeconds(records: Iterable<WitnessRecord>): number {
  let total = 0;
  for (const rec of records) {
    if (countsTowardCanonicalGpuSeconds(rec) && rec.gpu_seconds !== null) {
      total += rec.gpu_seconds;
    }
  }
  return total;
}

/** True when a verdict string is one of the documented enum members (loose consumers tolerate more). */
export function isKnownVerdict(v: string): v is Verdict {
  return (VERDICTS as readonly string[]).includes(v);
}

/** True when a labor-class string is one of the enum members. */
export function isKnownLaborClass(v: string): v is LaborClass {
  return (LABOR_CLASSES as readonly string[]).includes(v);
}
