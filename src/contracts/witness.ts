// tally — the witness ledger record (SPEC "Record schema", "Per-line hash chain"; PS#9/#10a;
// IMPLEMENTATION-PLAN §3 Witness, M1.2).
//
// The witness JSONL is the canonical, permanent, git-independent proof-of-labor — ledger-as-truth.
// This file owns the RECORD SHAPE and the hash-input CANONICALIZATION RULE (the one place both the
// writer `chain.ts` and the daemonless `verify.ts` agree on what bytes get hashed). The sha256
// implementation itself lives in M1.2; contracts defines the rule so there can be no disagreement.

import type { LaborClass, Pool, Verdict } from "./job";

/**
 * The trust-class tag on a charge (SPEC "Record schema" `charge`): GPU is verifiable proof;
 * subscription/API is annotation, never the proof.
 */
export type ChargeClass = "verifiable" | "annotation";
export const CHARGE_CLASSES = ["verifiable", "annotation"] as const satisfies readonly ChargeClass[];

/** A trust-class-tagged charge (SPEC "Record schema"). */
export interface Charge {
  unit: string;
  amount: number;
  class: ChargeClass;
}

/**
 * The full canonical, on-disk witness record — all 17+ fields (SPEC "Record schema"; PS#9 + jul9
 * chain fields). `pool`/`charge` are additive and altitude-preserving, populated GPU-only in v0
 * (IMPLEMENTATION-PLAN §1 item 6). Every heavy unit emits a line — row or no row.
 */
export interface WitnessRecord {
  /** Anchor; the same UUID journald and taskwarrior key on. Null for a rowless heavy unit. */
  task_uuid: string | null;
  /** One line per transition (ISO-8601). */
  transition_timestamp: string;
  verdict: Verdict;
  exit_code: number;
  /** Content hash of the output artifact(s); null when no artifact (e.g. clean-exit-no-artifact). */
  artifact_content_hash: string | null;
  /** Metered GPU-seconds derived from the witness span; absent (null) on cloud runs. */
  gpu_seconds: number | null;
  /** Wall-clock duration in seconds. */
  wall_clock: number;
  attempt: number;
  /** Monotonic fencing token (pls lease generation). */
  lease_epoch: number;
  /** Existence key for skip-if-already-done. */
  dedup_key: string | null;
  /** `fresh|recovered|reused`; non-`fresh` excluded from canonical GPU-seconds. */
  labor_class: LaborClass;
  /** Optional pi-RPC trace pointer; absent on opaque runs. */
  trace_ref?: string | null;
  /** The compute pool that served the unit (day-1: GPU only). */
  pool: Pool | null;
  /** Trust-class-tagged charge (GPU verifiable; subscription/API annotation). */
  charge: Charge | null;
  /** Executing model as a models.dev `provider/model-name` id; absent (null) on shell runs. */
  model: string | null;
  // --- per-line hash chain (jul9) ---
  /** Monotonic sequence number across the ledger-wide chain. */
  seq: number;
  /** `sha256:<hex>` of the prior ledger line. The genesis line uses `GENESIS_PREV_HASH`. */
  prev_hash: string;
  /** `"sha256:" + hex(sha256(the line's JSON with the hash field cleared))`. */
  hash: string;
}

/**
 * The 5-field projection form (SPEC "Record schema": the projection, never the stored shape).
 * A read-time view, never persisted.
 */
export interface WitnessProjection {
  task_uuid: string | null;
  gpu_seconds: number | null;
  artifact_hash: string | null;
  exit_code: number;
  evidence_checks: string[];
}

/** The `prev_hash` value of the genesis (first) line — no predecessor exists. */
export const GENESIS_PREV_HASH = "sha256:" + "0".repeat(64);

/** The literal-typed prefix every chain hash carries. */
export const HASH_PREFIX = "sha256:" as const;

/**
 * The hash-input CANONICALIZATION RULE (SPEC "Per-line hash chain"; IMPLEMENTATION-PLAN §3 Witness).
 * The hashed bytes are the line's own JSON with the `hash` field CLEARED (set to the empty string),
 * with all OTHER fields — including `seq` and `prev_hash` — present. This is the single definition
 * both `chain.ts` (writer) and `verify.ts` (daemonless verifier) MUST use, so they can never
 * disagree on what bytes are hashed.
 *
 * Serialization is `JSON.stringify` over the record with `hash:""`. Key order is the record's own
 * insertion order; the writer and verifier both build the object the same way, so the string is
 * stable. Returns the exact UTF-8 string whose sha256 becomes the `hash`.
 */
export function canonicalHashInput(record: WitnessRecord): string {
  return JSON.stringify({ ...record, hash: "" });
}

/**
 * Project a full record to the 5-field form (SPEC). `evidence_checks` is supplied by the caller
 * (it is not stored on the record — it is reconstructed from the job's `EvidenceCheck[]`).
 */
export function toProjection(record: WitnessRecord, evidenceChecks: string[] = []): WitnessProjection {
  return {
    task_uuid: record.task_uuid,
    gpu_seconds: record.gpu_seconds,
    artifact_hash: record.artifact_content_hash,
    exit_code: record.exit_code,
    evidence_checks: evidenceChecks,
  };
}

/**
 * Canonical-GPU-seconds inclusion rule (SPEC "Record schema", "Evidence gate"): a line counts
 * toward canonical GPU-seconds ONLY when it is `labor_class:fresh` AND its verdict is not
 * `clean-exit-no-artifact`. `reused`/`recovered` lines and gate-fails are excluded. Used by the
 * aggregation helpers in M1.2 and by `query standup`.
 */
export function countsTowardCanonicalGpuSeconds(record: WitnessRecord): boolean {
  return record.labor_class === "fresh" && record.verdict !== "clean-exit-no-artifact";
}
